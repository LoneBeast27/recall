//! One source audio stream, decoded and edited on demand: [`TrackReader::fill`] decodes just
//! enough packets to queue the next output samples of the kept ranges (`pcm_stream`).
//! Frames are placed on the source timeline by `frame_placer`; an undecodable packet is
//! skipped (`bad_audio`) and its time stays silent.

use std::collections::VecDeque;
use std::path::Path;

use ff::Rescale;
use ffmpeg_next as ff;

use super::bad_audio::BadPackets;
use super::edit_pcm::{gain_multiplier, range_sample_bounds};
use super::frame_placer::{self, Placer};
use super::pcm_stream::{take, OutQueue, RangeEditor, SourceWindow};
use super::timeline;
use crate::edit_model::{AudioSetting, TimeRange};

type Resampler = ff::software::resampling::Context;

pub(crate) struct TrackReader {
    decode: Decode,
    placer: Placer,
    window: SourceWindow,
    editor: RangeEditor,
    out: OutQueue,
    gain: f32,
}

/// The decoding side: packets in, resampled frames placed into a [`Placer`].
struct Decode {
    ictx: ff::format::context::Input,
    decoder: ff::decoder::Audio,
    resampler: Option<Resampler>,
    stream_index: usize,
    stream_tb: ff::Rational,
    rate: u32,
    layout: ff::ChannelLayout,
    channels: usize,
    origin_samples: i64,
    origin_us: i64,
    /// Packets past this (editor time) are never needed.
    stop_us: i64,
    /// Where the previous frame ended; the first frame goes to its timestamp.
    next_source_sample: Option<i64>,
    /// A frame failed to decode on the receive side: its length is unknown, so the next frame
    /// goes to its own timestamp.
    reseed: bool,
    bad: BadPackets,
}

impl TrackReader {
    /// Opens `stream_index` of `input` at its own rate and layout, or at `target`.
    pub fn open(
        input: &Path,
        origin_us: i64,
        ranges: &[TimeRange],
        stream_index: usize,
        setting: Option<&AudioSetting>,
        target: Option<(u32, ff::ChannelLayout)>,
    ) -> Result<Self, String> {
        let mut ictx = ff::format::input(input).map_err(|e| format!("open input for audio stream {stream_index}: {e}"))?;
        let (parameters, stream_tb) = {
            let stream = ictx.stream(stream_index).ok_or_else(|| format!("audio stream {stream_index} disappeared"))?;
            (stream.parameters(), stream.time_base())
        };
        let decoder = ff::codec::context::Context::from_parameters(parameters)
            .map_err(|e| format!("create audio decoder for stream {stream_index}: {e}"))?
            .decoder()
            .audio()
            .map_err(|e| format!("open audio decoder for stream {stream_index}: {e}"))?;
        if decoder.rate() == 0 {
            return Err(format!("audio stream {stream_index} has zero sample rate"));
        }
        let mut layout = decoder.channel_layout();
        if layout.is_empty() {
            layout = ff::ChannelLayout::default(decoder.channels().max(1) as i32);
        }
        let (rate, layout) = target.unwrap_or((decoder.rate(), layout));
        let channels = layout.channels().max(1) as usize;
        let bounds = range_sample_bounds(ranges, rate)?;
        let (first, last) = (bounds.first().map_or(0, |b| b.source_start), bounds.last().map_or(0, |b| b.source_end));
        // Read only around the kept ranges; the margin keeps decoder warm-up out of them.
        let stop_us = if timeline::trim_decode() && !ranges.is_empty() {
            timeline::seek_near(&mut ictx, origin_us, timeline::seconds_to_us(ranges[0].start)?);
            let end = ranges.iter().map(|r| r.end).fold(0.0, f64::max);
            timeline::seconds_to_us(end)?.saturating_add(timeline::SEEK_MARGIN_US)
        } else {
            i64::MAX
        };
        let decode = Decode {
            ictx,
            decoder,
            resampler: None,
            stream_index,
            stream_tb,
            rate,
            layout,
            channels,
            origin_samples: origin_us.rescale(ff::rescale::TIME_BASE, ff::Rational(1, rate as i32)),
            origin_us,
            stop_us,
            next_source_sample: None,
            reseed: false,
            bad: BadPackets::default(),
        };
        Ok(Self {
            decode,
            placer: Placer::new(rate as i64),
            window: SourceWindow::new(channels, first, last),
            editor: RangeEditor::new(bounds, rate, layout),
            out: vec![VecDeque::new(); channels],
            gain: setting.map_or(1.0, |value| gain_multiplier(value.gain_db, value.muted)),
        })
    }

    pub fn rate(&self) -> u32 {
        self.decode.rate
    }

    pub fn layout(&self) -> ff::ChannelLayout {
        self.decode.layout
    }

    /// Output samples queued.
    pub fn available(&self) -> usize {
        frame_placer::queued_samples(&self.out)
    }

    /// Every output sample has been produced and taken.
    pub fn exhausted(&self) -> bool {
        self.editor.done() && self.available() == 0
    }

    pub fn take(&mut self, count: usize) -> Vec<Vec<f32>> {
        take(&mut self.out, count)
    }

    /// Decodes until `want` output samples are queued or the track has ended.
    pub fn fill(&mut self, want: usize) -> Result<(), String> {
        let decode = &mut self.decode;
        frame_placer::fill(&mut self.placer, &mut self.window, &mut self.editor, &mut self.out, self.gain, want, |placer| {
            decode.more(placer)
        })
    }
}

impl Decode {
    /// Reads packets until a new frame is queued or the input (or the needed part of it) ended.
    fn more(&mut self, placer: &mut Placer) -> Result<(), String> {
        let index = self.stream_index;
        let what = format!("decode audio packet for stream {index}");
        let queued = placer.queued();
        loop {
            let mut packet = ff::Packet::empty();
            match packet.read(&mut self.ictx) {
                Ok(()) => {}
                Err(ff::Error::Eof) => return self.end_input(placer),
                Err(_) => continue, // as `Input::packets` does: skip an unreadable packet
            }
            if packet.stream() != index {
                continue;
            }
            let packet_us = packet
                .pts()
                .or(packet.dts())
                .map(|pts| pts.rescale(self.stream_tb, ff::rescale::TIME_BASE).saturating_sub(self.origin_us));
            if packet_us.is_some_and(|us| us > self.stop_us) {
                return self.end_input(placer);
            }
            // One undecodable packet is skipped; its time stays silent (the next frame goes
            // after it, not closed up against the previous one).
            self.bad.send(&mut self.decoder, &packet, &what)?;
            let gap = self.bad.take_gap().rescale(self.stream_tb, ff::Rational(1, self.rate as i32));
            if gap > 0 {
                self.next_source_sample = self.next_source_sample.map(|next| next.saturating_add(gap));
            }
            self.receive_frames(placer)?;
            if placer.queued() > queued {
                return Ok(());
            }
        }
    }

    fn end_input(&mut self, placer: &mut Placer) -> Result<(), String> {
        let index = self.stream_index;
        self.decoder.send_eof().map_err(|e| format!("flush audio decoder for stream {index}: {e}"))?;
        self.receive_frames(placer)?;
        self.flush_resampler(placer)?;
        self.bad.report(&format!("audio stream {index}"));
        placer.end_input();
        Ok(())
    }

    fn receive_frames(&mut self, placer: &mut Placer) -> Result<(), String> {
        let mut decoded = ff::frame::Audio::empty();
        loop {
            match self.decoder.receive_frame(&mut decoded) {
                Ok(()) => self.convert(&decoded, placer)?,
                Err(error) if super::receive_is_drained(&error) => return Ok(()),
                Err(error) => self.receive_failed(error)?,
            }
        }
    }

    /// A frame the decoder could not produce: skipped like a rejected packet (`bad_audio`), and
    /// the next frame goes to its own timestamp, since this one's length is unknown.
    fn receive_failed(&mut self, error: ff::Error) -> Result<(), String> {
        let index = self.stream_index;
        self.bad.skip(error, &format!("receive decoded audio frame for stream {index}"))?;
        self.reseed = true;
        Ok(())
    }

    /// Resamples one decoded frame and queues it at its place on the source timeline.
    fn convert(&mut self, decoded: &ff::frame::Audio, placer: &mut Placer) -> Result<(), String> {
        let index = self.stream_index;
        let mut input_layout = decoded.channel_layout();
        if input_layout.is_empty() {
            input_layout = ff::ChannelLayout::default(decoded.channels().max(1) as i32);
        }
        let input_rate = decoded.rate().max(1);
        let stale = self.resampler.as_ref().is_none_or(|current| {
            current.input().format != decoded.format() || current.input().channel_layout != input_layout || current.input().rate != input_rate
        });
        if stale {
            self.flush_resampler(placer)?;
            let format = ff::format::Sample::F32(ff::format::sample::Type::Planar);
            let created = Resampler::get(decoded.format(), input_layout, input_rate, format, self.layout, self.rate)
                .map_err(|e| format!("create resampler for audio stream {index}: {e}"))?;
            self.resampler = Some(created);
        }
        let mut converted = ff::frame::Audio::empty();
        self.resampler
            .as_mut()
            .expect("resampler initialized")
            .run(decoded, &mut converted)
            .map_err(|e| format!("resample audio stream {index}: {e}"))?;
        let stamped = decoded
            .timestamp()
            .or(decoded.pts())
            .map(|pts| pts.rescale(self.stream_tb, ff::Rational(1, self.rate as i32)).saturating_sub(self.origin_samples));
        // Back to back unless the stamps show a real gap (see `audio_clock`); after a frame lost
        // on the receive side, at its own stamp when it has one.
        let tolerance = crate::audio_clock::drift_tolerance(self.rate);
        let start = match (std::mem::take(&mut self.reseed), stamped) {
            (true, Some(stamped)) => stamped,
            _ => crate::audio_clock::frame_start(self.next_source_sample, stamped, tolerance).0,
        };
        self.queue(start, &converted, placer);
        Ok(())
    }

    fn flush_resampler(&mut self, placer: &mut Placer) -> Result<(), String> {
        let index = self.stream_index;
        let Some(resampler) = self.resampler.as_mut() else { return Ok(()) };
        let output = *resampler.output();
        let mut tails = Vec::new();
        loop {
            let delay = resampler.delay().map_or(0, |delay| delay.output.max(0) as usize);
            let mut delayed = ff::frame::Audio::new(output.format, delay.saturating_add(64), output.channel_layout);
            delayed.set_rate(output.rate);
            resampler.flush(&mut delayed).map_err(|e| format!("flush resampler for audio stream {index}: {e}"))?;
            if delayed.samples() == 0 {
                break;
            }
            tails.push(delayed);
        }
        for delayed in tails {
            let at = self.next_source_sample.unwrap_or(0);
            self.queue(at, &delayed, placer);
        }
        Ok(())
    }

    fn queue(&mut self, start: i64, frame: &ff::frame::Audio, placer: &mut Placer) {
        let samples = frame.samples();
        let planes = (0..self.channels).map(|channel| frame.plane::<f32>(channel)[..samples].to_vec()).collect();
        placer.push(start, self.next_source_sample, planes);
        self.next_source_sample = Some(start.saturating_add(samples as i64));
    }
}

#[cfg(test)]
#[path = "track_reader_tests.rs"]
mod tests;
