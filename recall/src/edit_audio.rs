//! Decode, edit, and AAC-encode the audio streams used by edited exports. Every track is
//! streamed in chunks (`export::track_reader`), so an export of any length holds about a
//! second of PCM per track; only the encoded AAC packets are kept whole.

use std::path::Path;

use ffmpeg_next as ff;

use crate::edit_model::{AudioOutput, EditPlan};
use crate::export::mix_limit::stream::StreamLimiter;
use crate::export::track_reader::TrackReader;

pub(crate) struct EncodedAudioTrack {
    /// The source stream (the first one for a mix): its metadata names the track.
    pub(crate) source_index: usize,
    /// Several streams summed: the output track is named "Mix".
    pub(crate) mixed: bool,
    pub(crate) rate: u32,
    pub(crate) encoder: ff::encoder::Audio,
    pub(crate) packets: Vec<ff::Packet>,
}

impl EncodedAudioTrack {
    /// Tags for the output stream: the source stream's, or "Mix" for a mixed track.
    pub(crate) fn metadata(&self, input: &ff::format::context::Input) -> Option<ff::Dictionary<'static>> {
        if self.mixed {
            let mut tags = ff::Dictionary::new();
            tags.set("handler_name", MIX_NAME);
            tags.set("title", MIX_NAME);
            return Some(tags);
        }
        input.stream(self.source_index).map(|stream| stream.metadata().to_owned())
    }
}

/// Name of the track that carries the mixed sources.
pub(crate) const MIX_NAME: &str = "Mix";
/// A mix is decoded to this rate and layout before the tracks are summed.
const MIX_RATE: u32 = 48_000;
/// Output samples moved per step (one second at 48 kHz).
const CHUNK: usize = 48_000;

/// The audio streams of `input`, in stream order.
pub(crate) fn audio_indices(input: &Path) -> Result<Vec<usize>, String> {
    let probe = ff::format::input(input)
        .map_err(|e| format!("open input audio {}: {e}", input.display()))?;
    Ok(probe
        .streams()
        .filter(|s| s.parameters().medium() == ff::media::Type::Audio)
        .map(|s| s.index())
        .collect())
}

/// Edit the audio of `input` per `plan`, as the tracks the export writes (mix first).
pub(crate) fn encode_edited_audio(
    input: &Path,
    origin_us: i64,
    plan: &EditPlan,
) -> Result<Vec<EncodedAudioTrack>, String> {
    let outputs = plan.audio_outputs(&audio_indices(input)?);
    encode_outputs(input, origin_us, plan, &outputs)
}

/// Edit and encode the listed output tracks (each range time-stretched by its speed).
pub(crate) fn encode_outputs(
    input: &Path,
    origin_us: i64,
    plan: &EditPlan,
    outputs: &[AudioOutput],
) -> Result<Vec<EncodedAudioTrack>, String> {
    let setting = |index: usize| plan.audio.iter().find(|s| s.stream_index == index);
    outputs
        .iter()
        .map(|output| match output {
            AudioOutput::Track(index) => {
                let mut reader = TrackReader::open(input, origin_us, &plan.ranges, *index, setting(*index), None)?;
                let mut sink = AacSink::open(*index, reader.rate(), reader.layout())?;
                loop {
                    reader.fill(CHUNK)?;
                    if reader.exhausted() {
                        break;
                    }
                    let count = reader.available();
                    sink.push(&reader.take(count))?;
                }
                sink.finish(false)
            }
            AudioOutput::Mix(indices) => encode_mix(input, origin_us, plan, indices),
        })
        .collect()
}

/// Sum the edited tracks (each with its own gain) into one 48 kHz stereo track, then
/// level it (`mix_limit`). Only called for two or more tracks: a lone track is never touched.
fn encode_mix(input: &Path, origin_us: i64, plan: &EditPlan, indices: &[usize]) -> Result<EncodedAudioTrack, String> {
    let first = *indices.first().ok_or("a mix needs at least one track")?;
    let target = (MIX_RATE, ff::ChannelLayout::STEREO);
    let mut readers = indices
        .iter()
        .map(|&index| {
            let setting = plan.audio.iter().find(|s| s.stream_index == index);
            TrackReader::open(input, origin_us, &plan.ranges, index, setting, Some(target))
        })
        .collect::<Result<Vec<_>, _>>()?;
    let mut sink = AacSink::open(first, target.0, target.1)?;
    // Unity-gain sum; two loud sources can pass full scale, so a limiter, never a hard clip.
    let mut limiter = StreamLimiter::new(2, target.0);
    loop {
        for reader in &mut readers {
            reader.fill(CHUNK)?;
        }
        let Some(count) = readers.iter().filter(|r| !r.exhausted()).map(TrackReader::available).min() else { break };
        let mut sum = vec![vec![0.0f32; count]; 2];
        for reader in &mut readers {
            let part = reader.take(count);
            add_into(&mut sum, &part);
        }
        sink.push(&planar(&limiter.push(&interleave(&sum)), 2))?;
    }
    sink.push(&planar(&limiter.finish(), 2))?;
    sink.finish(true)
}

/// Adds `pcm` channel by channel onto `sum`, growing it to the longer length.
pub(crate) fn add_into(sum: &mut Vec<Vec<f32>>, pcm: &[Vec<f32>]) {
    if sum.len() < pcm.len() {
        sum.resize_with(pcm.len(), Vec::new);
    }
    for (total, channel) in sum.iter_mut().zip(pcm) {
        if total.len() < channel.len() {
            total.resize(channel.len(), 0.0);
        }
        total.iter_mut().zip(channel).for_each(|(t, s)| *t += s);
    }
}

fn interleave(planes: &[Vec<f32>]) -> Vec<f32> {
    let frames = planes.first().map_or(0, Vec::len);
    (0..frames).flat_map(|n| planes.iter().map(move |plane| plane[n])).collect()
}

fn planar(samples: &[f32], channels: usize) -> Vec<Vec<f32>> {
    (0..channels).map(|c| samples.iter().skip(c).step_by(channels).copied().collect()).collect()
}

/// The AAC encoder of one output track, fed planar chunks of any size.
struct AacSink {
    source_index: usize,
    rate: u32,
    layout: ff::ChannelLayout,
    encoder: ff::encoder::Audio,
    frame_size: usize,
    /// Samples not yet sent (under one frame).
    carry: Vec<Vec<f32>>,
    position: usize,
    packets: Vec<ff::Packet>,
}

impl AacSink {
    fn open(source_index: usize, rate: u32, layout: ff::ChannelLayout) -> Result<Self, String> {
        let codec = ff::encoder::find(ff::codec::Id::AAC).ok_or("AAC encoder is unavailable")?;
        let mut context = ff::codec::context::Context::new_with_codec(codec)
            .encoder()
            .audio()
            .map_err(|e| format!("create AAC encoder for stream {source_index}: {e}"))?;
        context.set_rate(rate as i32);
        context.set_format(ff::format::Sample::F32(ff::format::sample::Type::Planar));
        context.set_channel_layout(layout);
        context.set_bit_rate(96_000usize.saturating_mul(layout.channels().max(1) as usize));
        context.set_time_base(ff::Rational(1, rate as i32));
        context.set_flags(ff::codec::Flags::GLOBAL_HEADER);
        let encoder = context
            .open()
            .map_err(|e| format!("open AAC encoder for stream {source_index}: {e}"))?;
        let frame_size = encoder.frame_size().max(1) as usize;
        let channels = layout.channels().max(1) as usize;
        Ok(Self { source_index, rate, layout, encoder, frame_size, carry: vec![Vec::new(); channels], position: 0, packets: Vec::new() })
    }

    fn push(&mut self, planes: &[Vec<f32>]) -> Result<(), String> {
        for (carry, plane) in self.carry.iter_mut().zip(planes) {
            carry.extend_from_slice(plane);
        }
        let ready = self.carry.first().map_or(0, Vec::len) / self.frame_size * self.frame_size;
        let mut at = 0;
        while at < ready {
            self.send(at, self.frame_size)?;
            at += self.frame_size;
        }
        for carry in &mut self.carry {
            carry.drain(..ready);
        }
        Ok(())
    }

    /// Sends `count` carried samples from `at` as one frame (zero-padded to the frame size).
    fn send(&mut self, at: usize, count: usize) -> Result<(), String> {
        let format = ff::format::Sample::F32(ff::format::sample::Type::Planar);
        let mut frame = ff::frame::Audio::new(format, self.frame_size, self.layout);
        frame.set_rate(self.rate);
        frame.set_samples(count);
        frame.set_pts(Some(self.position as i64));
        for (channel, source) in self.carry.iter().enumerate() {
            let destination = frame.plane_mut::<f32>(channel);
            destination[..count].copy_from_slice(&source[at..at + count]);
            destination[count..].fill(0.0);
        }
        self.encoder
            .send_frame(&frame)
            .map_err(|e| format!("send PCM to AAC encoder for stream {}: {e}", self.source_index))?;
        self.position += count;
        self.drain()
    }

    fn finish(mut self, mixed: bool) -> Result<EncodedAudioTrack, String> {
        let left = self.carry.first().map_or(0, Vec::len);
        if left > 0 {
            self.send(0, left)?;
        }
        self.encoder
            .send_eof()
            .map_err(|e| format!("flush AAC encoder for stream {}: {e}", self.source_index))?;
        self.drain()?;
        Ok(EncodedAudioTrack { source_index: self.source_index, mixed, rate: self.rate, encoder: self.encoder, packets: self.packets })
    }

    fn drain(&mut self) -> Result<(), String> {
        loop {
            let mut packet = ff::Packet::empty();
            match self.encoder.receive_packet(&mut packet) {
                Ok(()) => self.packets.push(packet),
                Err(error) if crate::export::receive_is_drained(&error) => return Ok(()),
                Err(error) => return Err(format!("receive AAC packet for stream {}: {error}", self.source_index)),
            }
        }
    }
}
