//! Whole-clip VFR-to-CFR re-encode; every audio stream is remuxed unchanged
//! with rebased timestamps.

use std::path::Path;

use ff::Rescale;
use ffmpeg_next as ff;

use super::receive_is_drained;
use super::timeline::{ticks_before, timeline_duration_us, timeline_origin_us};
use super::video_encoder::{
    add_video_stream, nvenc_codec, open_encoder, output_dims, probe_video, recommended_bitrate,
    source_bitrate_ceiling,
    EncoderSpec, RateControl, VideoBackend,
};
use super::convert::{video_path, with_fallback, Convert};
use super::ExportResult;

pub(super) fn export_to_temp(input: &Path, temp: &Path, fps: u32) -> Result<ExportResult, String> {
    with_fallback(super::gpu::wanted(), |gpu| export_attempt(input, temp, fps, gpu))
}

/// One whole encode into `temp`, on the GPU path when `gpu`.
fn export_attempt(input: &Path, temp: &Path, fps: u32, gpu: bool) -> Result<ExportResult, String> {
    let mut ictx =
        ff::format::input(input).map_err(|e| format!("open input {}: {e}", input.display()))?;
    let source = probe_video(&ictx)?;
    let video_index = source.index;
    let (width, height) = output_dims(source.width, source.height, None);
    let (mut decoder, mut convert) = video_path(&source, width, height, gpu)?;
    let (encoder, codec) = open_encoder(&EncoderSpec {
        backend: VideoBackend::Nvenc,
        width,
        height,
        aspect: source.aspect,
        fps,
        rate: RateControl::quality({
            let quality = recommended_bitrate(width, height, fps);
            let codec = nvenc_codec(width, height).unwrap_or("h264_nvenc");
            source_bitrate_ceiling(&source, codec).map_or(quality, |ceiling| quality.min(ceiling))
        }),
        frames: convert.frames(),
    })?;
    let output_tb = ff::Rational(1, fps as i32);

    let mut octx = ff::format::output(temp)
        .map_err(|e| format!("create temporary output {}: {e}", temp.display()))?;
    // HEVC (frames past H.264 NVENC) is tagged `hvc1`, as in the edited export.
    let video_out_index = add_video_stream(&mut octx, codec, &encoder, output_tb)?;

    let mut stream_map = vec![None; ictx.nb_streams() as usize];
    let mut input_time_bases = vec![ff::Rational(0, 1); ictx.nb_streams() as usize];
    for stream in ictx.streams() {
        input_time_bases[stream.index()] = stream.time_base();
        if stream.parameters().medium() != ff::media::Type::Audio {
            continue;
        }
        let mut out = octx
            .add_stream(ff::encoder::find(ff::codec::Id::None))
            .map_err(|e| format!("add output audio stream {}: {e}", stream.index()))?;
        out.set_parameters(stream.parameters());
        out.set_metadata(stream.metadata().to_owned());
        unsafe {
            (*out.parameters().as_mut_ptr()).codec_tag = 0;
        }
        stream_map[stream.index()] = Some(out.index());
    }
    let origin_us = timeline_origin_us(&ictx, video_index);
    let planned_duration_us = timeline_duration_us(&ictx, origin_us);
    // Every audio track is kept as it is: the sources tag passes through.
    let sources = super::sources::unchanged(&ictx);
    super::sources::write_header(&mut octx, &ictx, sources, false)?;
    let output_time_bases: Vec<_> = octx.streams().map(|s| s.time_base()).collect();

    let mut pipeline = VideoPipeline {
        decoder: &mut decoder,
        convert: &mut convert,
        encoder,
        source_time_base: source.time_base,
        output_time_base: output_tb,
        output_stream: video_out_index,
        origin_us,
        held: None,
        cursor: 0,
        newest_frame_us: 0,
    };
    let mut observed_end_us = 0i64;

    for (stream, mut packet) in ictx.packets() {
        let stream_index = stream.index();
        let source_tb = input_time_bases[stream_index];
        if let Some(timestamp) = packet.pts().or(packet.dts()) {
            let end = timestamp.saturating_add(packet.duration().max(0));
            let end_us = end
                .rescale(source_tb, ff::rescale::TIME_BASE)
                .saturating_sub(origin_us);
            observed_end_us = observed_end_us.max(end_us);
        }

        if stream_index == video_index {
            pipeline.send_packet(&packet, &mut octx)?;
        } else if let Some(output_index) = stream_map[stream_index] {
            let origin_in_stream = origin_us.rescale(ff::rescale::TIME_BASE, source_tb);
            packet.set_pts(packet.pts().map(|v| v.saturating_sub(origin_in_stream)));
            packet.set_dts(packet.dts().map(|v| v.saturating_sub(origin_in_stream)));
            packet.rescale_ts(source_tb, output_time_bases[output_index]);
            packet.set_position(-1);
            packet.set_stream(output_index);
            packet
                .write_interleaved(&mut octx)
                .map_err(|e| format!("write audio stream {stream_index}: {e}"))?;
        }
    }

    pipeline.finish_decoder(&mut octx)?;
    let duration_us = planned_duration_us
        .max(observed_end_us)
        .max(pipeline.newest_frame_us);
    pipeline.emit_until(ticks_before(duration_us, fps), &mut octx)?;
    if pipeline.cursor == 0 {
        return Err("input video decoded no frames".into());
    }
    pipeline.finish_encoder(&mut octx)?;
    let frames = pipeline.cursor;
    drop(pipeline);
    octx.write_trailer()
        .map_err(|e| format!("write output trailer: {e}"))?;

    Ok(ExportResult {
        frames,
        seconds: frames as f64 / fps as f64,
    })
}

struct VideoPipeline<'a> {
    decoder: &'a mut ff::decoder::Video,
    convert: &'a mut Convert,
    encoder: ff::encoder::Video,
    source_time_base: ff::Rational,
    output_time_base: ff::Rational,
    output_stream: usize,
    origin_us: i64,
    held: Option<ff::frame::Video>,
    cursor: u64,
    newest_frame_us: i64,
}

impl VideoPipeline<'_> {
    fn send_packet(
        &mut self,
        packet: &ff::Packet,
        output: &mut ff::format::context::Output,
    ) -> Result<(), String> {
        self.decoder
            .send_packet(packet)
            .map_err(|e| format!("decode video packet: {e}"))?;
        self.receive_frames(output)
    }

    fn finish_decoder(&mut self, output: &mut ff::format::context::Output) -> Result<(), String> {
        self.decoder
            .send_eof()
            .map_err(|e| format!("flush video decoder: {e}"))?;
        self.receive_frames(output)
    }

    fn receive_frames(&mut self, output: &mut ff::format::context::Output) -> Result<(), String> {
        let mut decoded = ff::frame::Video::empty();
        loop {
            match self.decoder.receive_frame(&mut decoded) {
                Ok(()) => {
                    let timestamp = decoded.timestamp().or(decoded.pts()).ok_or_else(|| {
                        "decoded video frame has no timestamp; CFR sampling requires timestamps"
                            .to_string()
                    })?;
                    let frame_us = timestamp
                        .rescale(self.source_time_base, ff::rescale::TIME_BASE)
                        .saturating_sub(self.origin_us)
                        .max(0);
                    self.emit_until(
                        ticks_before(frame_us, self.output_time_base.1 as u32),
                        output,
                    )?;

                    self.held = Some(self.convert.run(&decoded, self.held.take())?);
                    self.newest_frame_us = self.newest_frame_us.max(frame_us);
                }
                Err(error) if receive_is_drained(&error) => break,
                Err(error) => return Err(format!("receive decoded video frame: {error}")),
            }
        }
        Ok(())
    }

    fn emit_until(
        &mut self,
        target: u64,
        output: &mut ff::format::context::Output,
    ) -> Result<(), String> {
        // Encoders take their own reference on send, so the held frame is re-sent with a new
        // pts per tick; a deep copy per tick (ffmpeg-next's `Video::clone`) cost two full-frame
        // allocations and copies per output frame. On error the export ends, frame and all.
        let Some(mut frame) = self.held.take() else {
            return Ok(());
        };
        while self.cursor < target {
            frame.set_pts(Some(self.cursor as i64));
            frame.set_kind(ff::picture::Type::None);
            self.encoder
                .send_frame(&frame)
                .map_err(|e| format!("send frame {} to h264_nvenc: {e}", self.cursor))?;
            self.cursor += 1;
            self.drain_encoder(output)?;
        }
        self.held = Some(frame);
        Ok(())
    }

    fn finish_encoder(&mut self, output: &mut ff::format::context::Output) -> Result<(), String> {
        self.encoder
            .send_eof()
            .map_err(|e| format!("flush h264_nvenc: {e}"))?;
        self.drain_encoder(output)
    }

    fn drain_encoder(&mut self, output: &mut ff::format::context::Output) -> Result<(), String> {
        let mut packet = ff::Packet::empty();
        loop {
            match self.encoder.receive_packet(&mut packet) {
                Ok(()) => {
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
                        .map_err(|e| format!("write encoded video packet: {e}"))?;
                }
                Err(error) if receive_is_drained(&error) => break,
                Err(error) => return Err(format!("receive h264_nvenc packet: {error}")),
            }
        }
        Ok(())
    }
}
