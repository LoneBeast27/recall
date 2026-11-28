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

/// The codec a re-encoded track is written in: AAC, or FLAC for a lossless export (a track MP4
/// can't copy, and the merged Mix track).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum TrackCodec {
    Aac,
    /// Lossless, for a lossless export of a track MP4 can't copy (PCM, Vorbis, TrueHD, DTS):
    /// 16-bit for a 16-bit (or 8-bit) source, else 24-bit.
    Flac { bits24: bool },
}

/// Edit and encode the listed output tracks (each range time-stretched by its speed).
pub(crate) fn encode_outputs(
    input: &Path,
    origin_us: i64,
    plan: &EditPlan,
    outputs: &[AudioOutput],
) -> Result<Vec<EncodedAudioTrack>, String> {
    encode_outputs_as(input, origin_us, plan, outputs, |_| TrackCodec::Aac)
}

/// [`encode_outputs`] with each output's codec chosen by the caller.
pub(crate) fn encode_outputs_as(
    input: &Path,
    origin_us: i64,
    plan: &EditPlan,
    outputs: &[AudioOutput],
    codec: impl Fn(&AudioOutput) -> TrackCodec,
) -> Result<Vec<EncodedAudioTrack>, String> {
    let setting = |index: usize| plan.audio.iter().find(|s| s.stream_index == index);
    outputs
        .iter()
        .map(|output| match output {
            AudioOutput::Track(index) => {
                let kind = codec(output);
                let mut reader = TrackReader::open(input, origin_us, &plan.ranges, *index, setting(*index), None)?;
                if let (TrackCodec::Aac, Some(rate)) = (kind, aac_rate_for(reader.rate())) {
                    // Above what AAC takes (192 kHz): the reader's resampler brings it down.
                    let layout = reader.layout();
                    reader = TrackReader::open(input, origin_us, &plan.ranges, *index, setting(*index), Some((rate, layout)))?;
                }
                let mut sink = AacSink::open_as(*index, reader.rate(), reader.layout(), kind)?;
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
            AudioOutput::Mix(indices) => encode_mix(input, origin_us, plan, indices, codec(output)),
        })
        .collect()
}

/// Sum the edited tracks (each with its own gain) into one 48 kHz stereo track, then
/// level it (`mix_limit`). Only called for two or more tracks: a lone track is never touched.
fn encode_mix(input: &Path, origin_us: i64, plan: &EditPlan, indices: &[usize], kind: TrackCodec) -> Result<EncodedAudioTrack, String> {
    let first = *indices.first().ok_or("a mix needs at least one track")?;
    let target = (MIX_RATE, ff::ChannelLayout::STEREO);
    let mut readers = indices
        .iter()
        .map(|&index| {
            let setting = plan.audio.iter().find(|s| s.stream_index == index);
            TrackReader::open(input, origin_us, &plan.ranges, index, setting, Some(target))
        })
        .collect::<Result<Vec<_>, _>>()?;
    let mut sink = AacSink::open_as(first, target.0, target.1, kind)?;
    // Unity-gain sum times the Mix level offset; two loud sources can pass full scale, so a
    // limiter ("Prevent clipping"), or with it off a clamp at full scale, never a wraparound.
    let level = plan.mix_level;
    let gain = level.gain();
    let mut limiter = level.limiter.then(|| StreamLimiter::new(2, target.0));
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
        let mixed = level_mix(&interleave(&sum), gain, limiter.is_none());
        match limiter.as_mut() {
            Some(limiter) => sink.push(&planar(limiter.push(&mixed), 2))?,
            None => sink.push(&planar(&mixed, 2))?,
        }
    }
    if let Some(limiter) = limiter.as_mut() {
        sink.push(&planar(limiter.finish(), 2))?;
    }
    sink.finish(true)
}

/// The summed Mix samples times the offset `gain`. With the limiter on they go to it as they
/// are (it holds them under -1 dBFS smoothly); off (`clamp`), a sum past full scale is clipped
/// at it, never wrapped by the encoder.
pub(crate) fn level_mix(samples: &[f32], gain: f32, clamp: bool) -> Vec<f32> {
    samples.iter().map(|s| if clamp { (s * gain).clamp(-1.0, 1.0) } else { s * gain }).collect()
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

/// The encoder's header (extradata) replaced by a packet's `NEW_EXTRADATA` side data, if any.
/// The output stream copies its parameters from the encoder after encoding, so it gets this one.
fn take_new_header(encoder: &mut ff::encoder::Audio, packet: &ff::Packet) {
    let Some(header) = packet.side_data().find(|d| d.kind() == ff::packet::side_data::Type::NewExtraData) else { return };
    let bytes = header.data();
    // SAFETY: an open encoder context; extradata is av_malloc'd with zeroed input padding, as
    // FFmpeg expects, and the old buffer is freed with av_free.
    unsafe {
        let context = encoder.as_mut_ptr();
        let padded = bytes.len() + ff::ffi::AV_INPUT_BUFFER_PADDING_SIZE as usize;
        let fresh = ff::ffi::av_mallocz(padded) as *mut u8;
        if fresh.is_null() {
            return;
        }
        std::ptr::copy_nonoverlapping(bytes.as_ptr(), fresh, bytes.len());
        ff::ffi::av_free((*context).extradata as *mut std::ffi::c_void);
        (*context).extradata = fresh;
        (*context).extradata_size = bytes.len() as i32;
    }
}

/// The highest AAC rate FFmpeg's encoder takes (it refused 192 kHz with "Invalid argument").
pub(crate) const AAC_MAX_RATE: u32 = 96_000;

/// The rate an AAC track of a `rate` source is encoded at, when the source's own is too high:
/// 88.2 kHz for the 44.1 kHz family (176.4, 352.8 kHz), else 96 kHz. None = the source's own.
pub(crate) fn aac_rate_for(rate: u32) -> Option<u32> {
    (rate > AAC_MAX_RATE).then_some(if rate.is_multiple_of(44_100) { 88_200 } else { AAC_MAX_RATE })
}

fn flac_format(bits24: bool) -> ff::format::Sample {
    use ff::format::sample::Type;
    if bits24 { ff::format::Sample::I32(Type::Packed) } else { ff::format::Sample::I16(Type::Packed) }
}

/// `count` samples of every carried channel from `at`, interleaved as 16-bit, or as 24 bits in
/// the top of 32 (FLAC's s32 layout). Exact for a source of that depth: its samples came through
/// f32, which holds 24 bits without loss. A float or 32-bit source is rounded to 24 bits.
fn interleave_ints(frame: &mut ff::frame::Audio, carry: &[Vec<f32>], at: usize, count: usize, bits24: bool) {
    let channels = carry.len();
    let (width, scale) = if bits24 { (4, 2_147_483_648.0f64) } else { (2, 32_768.0f64) };
    // SAFETY: a packed frame of `frame_size >= count` samples x `channels`, all in plane 0.
    let bytes = unsafe {
        let raw = frame.as_mut_ptr();
        std::slice::from_raw_parts_mut((*raw).data[0], (*raw).linesize[0] as usize)
    };
    for n in 0..count {
        for (c, source) in carry.iter().enumerate() {
            let value = (source[at + n] as f64 * scale).round();
            let offset = (n * channels + c) * width;
            if bits24 {
                // FLAC keeps the top 24 bits: round there, not by cutting the low byte.
                let v = ((value / 256.0).round() * 256.0).clamp(i32::MIN as f64, (i32::MAX - 255) as f64) as i32;
                bytes[offset..offset + 4].copy_from_slice(&v.to_le_bytes());
            } else {
                let v = value.clamp(i16::MIN as f64, i16::MAX as f64) as i16;
                bytes[offset..offset + 2].copy_from_slice(&v.to_le_bytes());
            }
        }
    }
}

fn planar(samples: &[f32], channels: usize) -> Vec<Vec<f32>> {
    (0..channels).map(|c| samples.iter().skip(c).step_by(channels).copied().collect()).collect()
}

/// The encoder of one output track (AAC, or FLAC for [`TrackCodec::Flac`]), fed planar chunks
/// of any size.
struct AacSink {
    codec: TrackCodec,
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
    fn open_as(source_index: usize, rate: u32, layout: ff::ChannelLayout, kind: TrackCodec) -> Result<Self, String> {
        if let TrackCodec::Flac { bits24 } = kind {
            return Self::open_flac(source_index, rate, layout, bits24);
        }
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
        Ok(Self { codec: kind, source_index, rate, layout, encoder, frame_size, carry: vec![Vec::new(); channels], position: 0, packets: Vec::new() })
    }

    /// FLAC at the source's depth: packed 16-bit, or packed 32-bit holding 24 bits.
    fn open_flac(source_index: usize, rate: u32, layout: ff::ChannelLayout, bits24: bool) -> Result<Self, String> {
        let codec = ff::encoder::find(ff::codec::Id::FLAC).ok_or("FLAC encoder is unavailable")?;
        let mut context = ff::codec::context::Context::new_with_codec(codec)
            .encoder()
            .audio()
            .map_err(|e| format!("create FLAC encoder for stream {source_index}: {e}"))?;
        context.set_rate(rate as i32);
        context.set_format(flac_format(bits24));
        context.set_channel_layout(layout);
        context.set_time_base(ff::Rational(1, rate as i32));
        context.set_flags(ff::codec::Flags::GLOBAL_HEADER);
        if bits24 {
            // SAFETY: a context not yet opened; FLAC reads the depth from this field.
            unsafe { (*context.as_mut_ptr()).bits_per_raw_sample = 24 };
        }
        let encoder = context.open().map_err(|e| format!("open FLAC encoder for stream {source_index}: {e}"))?;
        let frame_size = if encoder.frame_size() > 0 { encoder.frame_size() as usize } else { 4608 };
        let channels = layout.channels().max(1) as usize;
        let kind = TrackCodec::Flac { bits24 };
        Ok(Self { codec: kind, source_index, rate, layout, encoder, frame_size, carry: vec![Vec::new(); channels], position: 0, packets: Vec::new() })
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
        let format = match self.codec {
            TrackCodec::Aac => ff::format::Sample::F32(ff::format::sample::Type::Planar),
            TrackCodec::Flac { bits24 } => flac_format(bits24),
        };
        let mut frame = ff::frame::Audio::new(format, self.frame_size, self.layout);
        frame.set_rate(self.rate);
        frame.set_samples(count);
        frame.set_pts(Some(self.position as i64));
        match self.codec {
            TrackCodec::Aac => {
                for (channel, source) in self.carry.iter().enumerate() {
                    let destination = frame.plane_mut::<f32>(channel);
                    destination[..count].copy_from_slice(&source[at..at + count]);
                    destination[count..].fill(0.0);
                }
            }
            TrackCodec::Flac { bits24 } => interleave_ints(&mut frame, &self.carry, at, count, bits24),
        }
        self.encoder
            .send_frame(&frame)
            .map_err(|e| format!("send PCM to the encoder for stream {}: {e}", self.source_index))?;
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
            .map_err(|e| format!("flush the audio encoder for stream {}: {e}", self.source_index))?;
        self.drain()?;
        Ok(EncodedAudioTrack { source_index: self.source_index, mixed, rate: self.rate, encoder: self.encoder, packets: self.packets })
    }

    fn drain(&mut self) -> Result<(), String> {
        loop {
            let mut packet = ff::Packet::empty();
            match self.encoder.receive_packet(&mut packet) {
                // FLAC ends with an empty packet that only carries the final header (sample
                // count, MD5): it goes into the stream's header, and the muxer refuses the packet.
                Ok(()) if packet.size() == 0 => take_new_header(&mut self.encoder, &packet),
                Ok(()) => self.packets.push(packet),
                Err(error) if crate::export::receive_is_drained(&error) => return Ok(()),
                Err(error) => return Err(format!("receive audio packet for stream {}: {error}", self.source_index)),
            }
        }
    }
}
