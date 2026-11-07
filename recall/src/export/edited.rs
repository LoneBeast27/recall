//! Re-encoded edited export: gapless CFR video over the kept ranges (per-range
//! speed, optional downscale and size cap) plus one AAC stream per source
//! audio track (gain/mute, time-stretched with the video).

use std::path::Path;

use ff::Rescale;
use ffmpeg_next as ff;

use super::audio_mux::{payload_totals, EditedAudioMux};
use super::paths::file_bytes;
use super::project::{EditedExport, ExportedRange};
use super::receive_is_drained;
use super::size_cap::{self, CapInputs, MAX_ATTEMPTS};
use super::timeline::{
    seek_near, ticks_before, timeline_duration_us, timeline_origin_us, trim_decode,
    us_to_seconds, EditTimeline,
};
use super::video_encoder::{
    add_video_stream, open_decoder, open_encoder, output_dims, probe_video, recommended_bitrate, scaler,
    EncoderSpec, RateControl, SourceVideo, VideoBackend,
};
use crate::edit_audio::EncodedAudioTrack;
use crate::edit_model::{EditPlan, ExportMode, ExportSettings};

pub(super) fn export(
    input: &Path,
    temp: &Path,
    fps: u32,
    plan: &EditPlan,
    settings: &ExportSettings,
    backend: VideoBackend,
) -> Result<EditedExport, String> {
    let (source, origin_us, audio_indices, source_duration_us, sources) = {
        let ictx = ff::format::input(input)
            .map_err(|e| format!("open input {}: {e}", input.display()))?;
        let source = probe_video(&ictx)?;
        let origin_us = timeline_origin_us(&ictx, source.index);
        let duration_us = timeline_duration_us(&ictx, origin_us);
        let audio: Vec<_> = ictx
            .streams()
            .filter(|stream| stream.parameters().medium() == ff::media::Type::Audio)
            .map(|stream| stream.index())
            .collect();
        // The output's own sources tag: its tracks are the plan's outputs, in their order.
        let sources = super::sources::for_outputs(&ictx, &audio, &plan.audio_outputs(&audio));
        (source, origin_us, audio, duration_us, sources)
    };
    plan.validate(us_to_seconds(source_duration_us), &audio_indices)?;
    let timeline = EditTimeline::from_plan(plan)?;
    let (width, height) = output_dims(source.width, source.height, settings.height);
    let audio = crate::edit_audio::encode_edited_audio(input, origin_us, plan)?;
    let frames = ticks_before(timeline.output_end(), fps);
    let quality = recommended_bitrate(width, height, fps);
    let cap = settings.max_bytes.map(|max_bytes| {
        let (audio_bytes, audio_packets) = payload_totals(&audio);
        CapInputs {
            max_bytes,
            duration_s: us_to_seconds(timeline.output_end()),
            audio_bytes,
            video_frames: frames,
            audio_packets,
            quality_bitrate: quality,
        }
    });
    let mut rate = match &cap {
        Some(inputs) => size_cap::plan_rate(inputs)?,
        None => RateControl::quality(quality),
    };

    let attempt = Attempt {
        input,
        temp,
        fps,
        source: &source,
        origin_us,
        timeline: &timeline,
        audio: &audio,
        sources: &sources,
        width,
        height,
        backend,
        total: frames,
    };
    for number in 1..=MAX_ATTEMPTS {
        let written = attempt.encode(rate)?;
        let bytes = file_bytes(temp)?;
        if let Some(inputs) = cap.as_ref().filter(|inputs| bytes > inputs.max_bytes) {
            if number == MAX_ATTEMPTS {
                return Err(format!(
                    "the export came out at {:.2} MB, over the {:.2} MB cap, after {number} attempts; raise the cap or shorten the clip",
                    bytes as f64 / 1e6,
                    inputs.max_bytes as f64 / 1e6
                ));
            }
            rate = size_cap::tighten(rate, inputs, bytes)?;
            continue;
        }
        return Ok(EditedExport {
            mode: ExportMode::Reencode,
            frames: written,
            seconds: written as f64 / fps as f64,
            bytes,
            width,
            height,
            fps: Some(fps),
            video_bitrate: Some(rate.bitrate as u64),
            max_bytes: settings.max_bytes,
            attempts: number,
            ranges: reported_ranges(plan, &timeline),
        });
    }
    unreachable!("the attempt loop always returns")
}

fn reported_ranges(plan: &EditPlan, timeline: &EditTimeline) -> Vec<ExportedRange> {
    plan.ranges
        .iter()
        .zip(&timeline.ranges)
        .enumerate()
        .map(|(index, (range, mapped))| ExportedRange {
            start: range.start,
            end: range.end,
            speed: range.speed,
            output_start: us_to_seconds(mapped.output_start),
            output_end: us_to_seconds(mapped.output_end),
            plan_ranges: vec![index],
        })
        .collect()
}

struct Attempt<'a> {
    input: &'a Path,
    temp: &'a Path,
    fps: u32,
    source: &'a SourceVideo,
    origin_us: i64,
    timeline: &'a EditTimeline,
    audio: &'a [EncodedAudioTrack],
    /// `edit_model::AUDIO_SOURCES_TAG` for the output, when the input had one.
    sources: &'a Option<Vec<String>>,
    width: u32,
    height: u32,
    backend: VideoBackend,
    /// Output frames the plan asks for (progress reports count against it).
    total: u64,
}

impl Attempt<'_> {
    /// One full encode into the temp file (truncating it). Returns frames written.
    fn encode(&self, rate: RateControl) -> Result<u64, String> {
        let mut ictx = ff::format::input(self.input)
            .map_err(|e| format!("open input {}: {e}", self.input.display()))?;
        let mut decoder = open_decoder(self.source.params.clone())?;
        let mut scaler = scaler(self.source, self.width, self.height)?;
        let (encoder, codec) = open_encoder(&EncoderSpec {
            backend: self.backend,
            width: self.width,
            height: self.height,
            aspect: self.source.aspect,
            fps: self.fps,
            rate,
        })?;
        let output_tb = ff::Rational(1, self.fps as i32);
        let mut octx = ff::format::output(self.temp)
            .map_err(|e| format!("create temporary output {}: {e}", self.temp.display()))?;
        let video_out_index = add_video_stream(&mut octx, codec, &encoder, output_tb)?;
        let mut audio_output_indices = Vec::with_capacity(self.audio.len());
        for track in self.audio {
            let mut stream = octx
                .add_stream(ff::encoder::find(ff::codec::Id::AAC))
                .map_err(|e| format!("add output audio stream {}: {e}", track.source_index))?;
            stream.set_parameters(&track.encoder);
            stream.set_time_base(ff::Rational(1, track.rate as i32));
            if let Some(tags) = track.metadata(&ictx) {
                stream.set_metadata(tags);
            }
            audio_output_indices.push(stream.index());
        }
        super::sources::write_header(&mut octx, &ictx, self.sources.clone())?;
        let mut audio_mux = EditedAudioMux::new(self.audio, audio_output_indices);
        let mut pipeline = EditedVideoPipeline {
            decoder: &mut decoder,
            scaler: &mut scaler,
            encoder,
            source_time_base: self.source.time_base,
            output_time_base: output_tb,
            output_stream: video_out_index,
            origin_us: self.origin_us,
            timeline: self.timeline,
            range_index: 0,
            pending: None,
            held: None,
            cursor: 0,
            total: self.total,
            decoded_frames: 0,
        };
        // Start at the keyframe before the first kept range and stop once the last
        // range is filled: decoding the untouched rest of the clip only costs time.
        // The sampler ignores frames before a range, so the output is unchanged.
        let trim = trim_decode();
        if let Some(first) = self.timeline.ranges.first().filter(|_| trim) {
            seek_near(&mut ictx, self.origin_us, first.source_start);
        }
        let video_index = self.source.index;
        let mut packet = ff::Packet::empty();
        while !(trim && pipeline.ranges_done()) {
            match packet.read(&mut ictx) {
                Ok(()) => {}
                Err(ff::Error::Eof) => break,
                // As `Input::packets` does: a damaged packet is skipped, not fatal.
                Err(_) => continue,
            }
            if packet.stream() == video_index {
                pipeline.send_packet(&packet, &mut octx, &mut audio_mux)?;
            }
        }
        pipeline.finish_decoder(&mut octx, &mut audio_mux)?;
        pipeline.finish_ranges(&mut octx, &mut audio_mux)?;
        if pipeline.decoded_frames == 0 || pipeline.cursor == 0 {
            return Err("input video decoded no frames for the edit plan".into());
        }
        pipeline.finish_encoder(&mut octx, &mut audio_mux)?;
        #[cfg(test)]
        DECODED.with(|decoded| decoded.set(pipeline.decoded_frames));
        let frames = pipeline.cursor;
        drop(pipeline);
        audio_mux.finish(&mut octx)?;
        octx.write_trailer()
            .map_err(|e| format!("write output trailer: {e}"))?;
        Ok(frames)
    }
}

#[cfg(test)]
thread_local! {
    /// Frames the last export on this thread decoded.
    pub(super) static DECODED: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

struct EditedVideoPipeline<'a> {
    decoder: &'a mut ff::decoder::Video,
    scaler: &'a mut ff::software::scaling::Context,
    encoder: ff::encoder::Video,
    source_time_base: ff::Rational,
    output_time_base: ff::Rational,
    output_stream: usize,
    origin_us: i64,
    timeline: &'a EditTimeline,
    range_index: usize,
    /// Latest decoded frame, converted only once a tick actually needs it, so
    /// frames that the sampler drops (fast-forward, high source rates) are
    /// never scaled.
    pending: Option<ff::frame::Video>,
    held: Option<ff::frame::Video>,
    cursor: u64,
    total: u64,
    decoded_frames: u64,
}

type Mux<'m, 'a> = &'m mut EditedAudioMux<'a>;

/// Output frames between progress reports (half a second of 60 fps video).
const PROGRESS_EVERY: u64 = 30;

impl EditedVideoPipeline<'_> {
    fn fps(&self) -> u32 {
        self.output_time_base.1 as u32
    }

    /// Every range has seen a frame at or past its end: later frames add nothing.
    fn ranges_done(&self) -> bool {
        self.range_index >= self.timeline.ranges.len()
    }

    fn send_packet(
        &mut self,
        packet: &ff::Packet,
        output: &mut ff::format::context::Output,
        audio: Mux,
    ) -> Result<(), String> {
        self.decoder
            .send_packet(packet)
            .map_err(|e| format!("decode video packet: {e}"))?;
        self.receive_frames(output, audio)
    }

    fn finish_decoder(
        &mut self,
        output: &mut ff::format::context::Output,
        audio: Mux,
    ) -> Result<(), String> {
        self.decoder
            .send_eof()
            .map_err(|e| format!("flush video decoder: {e}"))?;
        self.receive_frames(output, audio)
    }

    fn receive_frames(
        &mut self,
        output: &mut ff::format::context::Output,
        audio: Mux,
    ) -> Result<(), String> {
        loop {
            let mut decoded = ff::frame::Video::empty();
            match self.decoder.receive_frame(&mut decoded) {
                Ok(()) => self.accept_frame(decoded, output, audio)?,
                Err(error) if receive_is_drained(&error) => break,
                Err(error) => return Err(format!("receive decoded video frame: {error}")),
            }
        }
        Ok(())
    }

    fn accept_frame(
        &mut self,
        decoded: ff::frame::Video,
        output: &mut ff::format::context::Output,
        audio: Mux,
    ) -> Result<(), String> {
        self.decoded_frames += 1;
        let timestamp = decoded.timestamp().or(decoded.pts()).ok_or_else(|| {
            "decoded video frame has no timestamp; edited CFR sampling requires timestamps"
                .to_string()
        })?;
        let frame_us = timestamp
            .rescale(self.source_time_base, ff::rescale::TIME_BASE)
            .saturating_sub(self.origin_us)
            .max(0);
        let ranges = &self.timeline.ranges;
        while self.range_index < ranges.len() && frame_us >= ranges[self.range_index].source_end {
            let target = ticks_before(ranges[self.range_index].output_end, self.fps());
            self.emit_until(target, output, audio)?;
            self.range_index += 1;
        }
        if let Some(range) = ranges.get(self.range_index).copied() {
            if frame_us >= range.source_start && frame_us < range.source_end {
                let target = ticks_before(range.output_time(frame_us), self.fps());
                self.emit_until(target, output, audio)?;
            }
        }
        self.pending = Some(decoded);
        Ok(())
    }

    fn finish_ranges(
        &mut self,
        output: &mut ff::format::context::Output,
        audio: Mux,
    ) -> Result<(), String> {
        while self.range_index < self.timeline.ranges.len() {
            let target = ticks_before(
                self.timeline.ranges[self.range_index].output_end,
                self.fps(),
            );
            self.emit_until(target, output, audio)?;
            self.range_index += 1;
        }
        Ok(())
    }

    fn convert_pending(&mut self) -> Result<(), String> {
        let Some(decoded) = self.pending.take() else {
            return Ok(());
        };
        let target = *self.scaler.output();
        let mut converted = ff::frame::Video::new(target.format, target.width, target.height);
        self.scaler
            .run(&decoded, &mut converted)
            .map_err(|e| format!("convert decoded video frame: {e}"))?;
        self.held = Some(converted);
        Ok(())
    }

    fn emit_until(
        &mut self,
        target: u64,
        output: &mut ff::format::context::Output,
        audio: Mux,
    ) -> Result<(), String> {
        if self.cursor >= target {
            return Ok(());
        }
        self.convert_pending()?;
        // Encoders copy input pictures on send, so one held frame is re-sent
        // with a new pts per tick instead of being cloned per tick.
        // An error aborts the whole export, so the frame is only put back on success.
        let Some(mut frame) = self.held.take() else {
            return Ok(());
        };
        while self.cursor < target {
            frame.set_pts(Some(self.cursor as i64));
            frame.set_kind(ff::picture::Type::None);
            self.encoder
                .send_frame(&frame)
                .map_err(|e| format!("send edited frame {} to encoder: {e}", self.cursor))?;
            self.cursor += 1;
            if self.cursor % PROGRESS_EVERY == 0 {
                super::report_progress(self.cursor, self.total);
            }
            self.drain_encoder(output, audio)?;
        }
        self.held = Some(frame);
        Ok(())
    }

    fn finish_encoder(
        &mut self,
        output: &mut ff::format::context::Output,
        audio: Mux,
    ) -> Result<(), String> {
        self.encoder
            .send_eof()
            .map_err(|e| format!("flush video encoder: {e}"))?;
        self.drain_encoder(output, audio)
    }

    fn drain_encoder(
        &mut self,
        output: &mut ff::format::context::Output,
        audio: Mux,
    ) -> Result<(), String> {
        let mut packet = ff::Packet::empty();
        loop {
            match self.encoder.receive_packet(&mut packet) {
                Ok(()) => {
                    let timestamp = packet.dts().or(packet.pts()).unwrap_or(0);
                    audio.write_before(
                        output,
                        timestamp as f64 * self.output_time_base.0 as f64
                            / self.output_time_base.1 as f64,
                    )?;
                    packet.set_stream(self.output_stream);
                    packet.set_duration(1);
                    packet.rescale_ts(
                        self.output_time_base,
                        output
                            .stream(self.output_stream)
                            .ok_or_else(|| "output video stream disappeared".to_string())?
                            .time_base(),
                    );
                    packet.set_position(-1);
                    packet
                        .write_interleaved(output)
                        .map_err(|e| format!("write encoded edited video packet: {e}"))?;
                }
                Err(error) if receive_is_drained(&error) => break,
                Err(error) => return Err(format!("receive encoded video packet: {error}")),
            }
        }
        Ok(())
    }
}
