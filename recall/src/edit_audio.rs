//! Decode, edit, and AAC-encode the audio streams used by edited exports.

use std::path::Path;

use ff::Rescale;
use ffmpeg_next as ff;

use crate::edit_model::{AudioSetting, EditPlan, TimeRange};

/// Edited PCM is retained only until it is fed to AAC. Bound it so a malformed
/// or unusually long project returns an error instead of risking process abort.
const MAX_EDITED_PCM_BYTES_PER_TRACK: usize = 256 * 1024 * 1024;

pub(crate) struct EncodedAudioTrack {
    pub(crate) source_index: usize,
    pub(crate) rate: u32,
    pub(crate) encoder: ff::encoder::Audio,
    pub(crate) packets: Vec<ff::Packet>,
}

pub(crate) fn encode_edited_audio(
    input: &Path,
    origin_us: i64,
    plan: &EditPlan,
) -> Result<Vec<EncodedAudioTrack>, String> {
    let probe = ff::format::input(input)
        .map_err(|e| format!("open input audio {}: {e}", input.display()))?;
    let indices: Vec<_> = probe
        .streams()
        .filter(|s| s.parameters().medium() == ff::media::Type::Audio)
        .map(|s| s.index())
        .collect();
    drop(probe);

    indices
        .into_iter()
        .map(|index| {
            let setting = plan
                .audio
                .iter()
                .find(|setting| setting.stream_index == index);
            encode_track(input, origin_us, &plan.ranges, index, setting)
        })
        .collect()
}

fn encode_track(
    input: &Path,
    origin_us: i64,
    ranges: &[TimeRange],
    stream_index: usize,
    setting: Option<&AudioSetting>,
) -> Result<EncodedAudioTrack, String> {
    let mut ictx = ff::format::input(input)
        .map_err(|e| format!("open input for audio stream {stream_index}: {e}"))?;
    let (parameters, stream_tb) = {
        let stream = ictx
            .stream(stream_index)
            .ok_or_else(|| format!("audio stream {stream_index} disappeared"))?;
        (stream.parameters(), stream.time_base())
    };
    let mut decoder = ff::codec::context::Context::from_parameters(parameters)
        .map_err(|e| format!("create audio decoder for stream {stream_index}: {e}"))?
        .decoder()
        .audio()
        .map_err(|e| format!("open audio decoder for stream {stream_index}: {e}"))?;
    let rate = decoder.rate();
    if rate == 0 {
        return Err(format!("audio stream {stream_index} has zero sample rate"));
    }
    let mut layout = decoder.channel_layout();
    if layout.is_empty() {
        layout = ff::ChannelLayout::default(decoder.channels().max(1) as i32);
    }
    let channels = layout.channels().max(1) as usize;
    let sample_format = ff::format::Sample::F32(ff::format::sample::Type::Planar);
    let bounds = range_sample_bounds(ranges, rate)?;
    let total_samples = bounds.last().map(|bound| bound.output_end).unwrap_or(0);
    let allocation = pcm_allocation_bytes(channels, total_samples)
        .ok_or_else(|| format!("edited audio stream {stream_index} is too large"))?;
    if allocation > MAX_EDITED_PCM_BYTES_PER_TRACK {
        return Err(format!(
            "edited audio stream {stream_index} requires {} MiB of PCM; the single-clip editor limit is 256 MiB per track",
            allocation.div_ceil(1024 * 1024)
        ));
    }
    let mut pcm = allocate_pcm(channels, total_samples, stream_index)?;
    let gain = setting.map_or(1.0, |value| gain_multiplier(value.gain_db, value.muted));
    let origin_samples = origin_us.rescale(ff::rescale::TIME_BASE, ff::Rational(1, rate as i32));
    let mut next_source_sample = 0i64;
    let mut resampler: Option<ff::software::resampling::Context> = None;

    for (stream, packet) in ictx.packets() {
        if stream.index() != stream_index {
            continue;
        }
        decoder
            .send_packet(&packet)
            .map_err(|e| format!("decode audio packet for stream {stream_index}: {e}"))?;
        receive_audio_frames(
            &mut decoder,
            &mut resampler,
            &mut pcm,
            &bounds,
            layout,
            rate,
            sample_format,
            stream_tb,
            origin_samples,
            gain,
            &mut next_source_sample,
            stream_index,
        )?;
    }
    decoder
        .send_eof()
        .map_err(|e| format!("flush audio decoder for stream {stream_index}: {e}"))?;
    receive_audio_frames(
        &mut decoder,
        &mut resampler,
        &mut pcm,
        &bounds,
        layout,
        rate,
        sample_format,
        stream_tb,
        origin_samples,
        gain,
        &mut next_source_sample,
        stream_index,
    )?;
    flush_resampler(
        &mut resampler,
        &mut pcm,
        &bounds,
        gain,
        &mut next_source_sample,
        stream_index,
    )?;

    encode_pcm(stream_index, rate, layout, pcm)
}

fn allocate_pcm(
    channels: usize,
    total_samples: usize,
    stream_index: usize,
) -> Result<Vec<Vec<f32>>, String> {
    let mut pcm = Vec::new();
    pcm.try_reserve_exact(channels).map_err(|error| {
        format!("reserve edited PCM channels for audio stream {stream_index}: {error}")
    })?;
    for _ in 0..channels {
        let mut channel = Vec::new();
        channel.try_reserve_exact(total_samples).map_err(|error| {
            format!(
                "reserve {} MiB edited PCM for audio stream {stream_index}: {error}",
                total_samples
                    .saturating_mul(std::mem::size_of::<f32>())
                    .div_ceil(1024 * 1024)
            )
        })?;
        channel.resize(total_samples, 0.0);
        pcm.push(channel);
    }
    Ok(pcm)
}

fn pcm_allocation_bytes(channels: usize, total_samples: usize) -> Option<usize> {
    total_samples
        .checked_mul(channels)
        .and_then(|samples| samples.checked_mul(std::mem::size_of::<f32>()))
}

#[allow(clippy::too_many_arguments)]
fn receive_audio_frames(
    decoder: &mut ff::decoder::Audio,
    resampler: &mut Option<ff::software::resampling::Context>,
    pcm: &mut [Vec<f32>],
    bounds: &[SampleRange],
    output_layout: ff::ChannelLayout,
    output_rate: u32,
    output_format: ff::format::Sample,
    stream_tb: ff::Rational,
    origin_samples: i64,
    gain: f32,
    next_source_sample: &mut i64,
    stream_index: usize,
) -> Result<(), String> {
    let mut decoded = ff::frame::Audio::empty();
    loop {
        match decoder.receive_frame(&mut decoded) {
            Ok(()) => {
                let mut input_layout = decoded.channel_layout();
                if input_layout.is_empty() {
                    input_layout = ff::ChannelLayout::default(decoded.channels().max(1) as i32);
                }
                let input_rate = decoded.rate().max(1);
                let needs_new_resampler = resampler.as_ref().is_none_or(|current| {
                    current.input().format != decoded.format()
                        || current.input().channel_layout != input_layout
                        || current.input().rate != input_rate
                });
                if needs_new_resampler {
                    flush_resampler(
                        resampler,
                        pcm,
                        bounds,
                        gain,
                        next_source_sample,
                        stream_index,
                    )?;
                    *resampler = Some(
                        ff::software::resampling::Context::get(
                            decoded.format(),
                            input_layout,
                            input_rate,
                            output_format,
                            output_layout,
                            output_rate,
                        )
                        .map_err(|e| {
                            format!("create resampler for audio stream {stream_index}: {e}")
                        })?,
                    );
                }
                let mut converted = ff::frame::Audio::empty();
                resampler
                    .as_mut()
                    .expect("resampler initialized")
                    .run(&decoded, &mut converted)
                    .map_err(|e| format!("resample audio stream {stream_index}: {e}"))?;
                let source_start = decoded
                    .timestamp()
                    .or(decoded.pts())
                    .map(|pts| {
                        pts.rescale(stream_tb, ff::Rational(1, output_rate as i32))
                            .saturating_sub(origin_samples)
                    })
                    .unwrap_or(*next_source_sample);
                place_frame(pcm, bounds, &converted, source_start, gain);
                *next_source_sample = source_start.saturating_add(converted.samples() as i64);
            }
            Err(error) if crate::export::receive_is_drained(&error) => break,
            Err(error) => {
                return Err(format!(
                    "receive decoded audio frame for stream {stream_index}: {error}"
                ))
            }
        }
    }
    Ok(())
}

fn flush_resampler(
    resampler: &mut Option<ff::software::resampling::Context>,
    pcm: &mut [Vec<f32>],
    bounds: &[SampleRange],
    gain: f32,
    next_source_sample: &mut i64,
    stream_index: usize,
) -> Result<(), String> {
    let Some(resampler) = resampler.as_mut() else {
        return Ok(());
    };
    let output = *resampler.output();
    loop {
        let delayed_samples = resampler
            .delay()
            .map(|delay| delay.output.max(0) as usize)
            .unwrap_or(0)
            .checked_add(64)
            .ok_or_else(|| {
                format!("resampler delay is too large for audio stream {stream_index}")
            })?;
        let mut delayed =
            ff::frame::Audio::new(output.format, delayed_samples, output.channel_layout);
        delayed.set_rate(output.rate);
        resampler
            .flush(&mut delayed)
            .map_err(|e| format!("flush resampler for audio stream {stream_index}: {e}"))?;
        let produced = delayed.samples();
        if produced == 0 {
            break;
        }
        place_frame(pcm, bounds, &delayed, *next_source_sample, gain);
        *next_source_sample = (*next_source_sample).saturating_add(produced as i64);
    }
    Ok(())
}

fn place_frame(
    pcm: &mut [Vec<f32>],
    bounds: &[SampleRange],
    frame: &ff::frame::Audio,
    source_start: i64,
    gain: f32,
) {
    let source_end = source_start.saturating_add(frame.samples() as i64);
    for range in bounds {
        let copy_start = source_start.max(range.source_start);
        let copy_end = source_end.min(range.source_end);
        if copy_end <= copy_start {
            continue;
        }
        let frame_offset = (copy_start - source_start) as usize;
        let output_offset = range.output_start + (copy_start - range.source_start) as usize;
        let count = (copy_end - copy_start) as usize;
        for (channel, destination) in pcm.iter_mut().enumerate() {
            let source = frame.plane::<f32>(channel);
            for index in 0..count {
                destination[output_offset + index] = source[frame_offset + index] * gain;
            }
        }
    }
}

fn encode_pcm(
    source_index: usize,
    rate: u32,
    layout: ff::ChannelLayout,
    pcm: Vec<Vec<f32>>,
) -> Result<EncodedAudioTrack, String> {
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
    let mut encoder = context
        .open()
        .map_err(|e| format!("open AAC encoder for stream {source_index}: {e}"))?;
    let frame_size = encoder.frame_size().max(1) as usize;
    let total = pcm.first().map(Vec::len).unwrap_or(0);
    let mut packets = Vec::new();
    let mut position = 0usize;
    while position < total {
        let count = frame_size.min(total - position);
        let mut frame = ff::frame::Audio::new(
            ff::format::Sample::F32(ff::format::sample::Type::Planar),
            frame_size,
            layout,
        );
        frame.set_rate(rate);
        frame.set_samples(count);
        frame.set_pts(Some(position as i64));
        for (channel, source) in pcm.iter().enumerate() {
            let destination = frame.plane_mut::<f32>(channel);
            destination[..count].copy_from_slice(&source[position..position + count]);
            destination[count..].fill(0.0);
        }
        encoder
            .send_frame(&frame)
            .map_err(|e| format!("send PCM to AAC encoder for stream {source_index}: {e}"))?;
        drain_packets(&mut encoder, &mut packets, source_index)?;
        position += count;
    }
    encoder
        .send_eof()
        .map_err(|e| format!("flush AAC encoder for stream {source_index}: {e}"))?;
    drain_packets(&mut encoder, &mut packets, source_index)?;
    Ok(EncodedAudioTrack {
        source_index,
        rate,
        encoder,
        packets,
    })
}

fn drain_packets(
    encoder: &mut ff::encoder::Audio,
    packets: &mut Vec<ff::Packet>,
    stream_index: usize,
) -> Result<(), String> {
    loop {
        let mut packet = ff::Packet::empty();
        match encoder.receive_packet(&mut packet) {
            Ok(()) => packets.push(packet),
            Err(error) if crate::export::receive_is_drained(&error) => break,
            Err(error) => {
                return Err(format!(
                    "receive AAC packet for stream {stream_index}: {error}"
                ))
            }
        }
    }
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct SampleRange {
    source_start: i64,
    source_end: i64,
    output_start: usize,
    output_end: usize,
}

fn range_sample_bounds(ranges: &[TimeRange], rate: u32) -> Result<Vec<SampleRange>, String> {
    let mut output_start = 0usize;
    ranges
        .iter()
        .map(|range| {
            let source_start = seconds_to_samples(range.start, rate)?;
            let source_end = seconds_to_samples(range.end, rate)?;
            let count = usize::try_from(source_end.saturating_sub(source_start))
                .map_err(|_| "edited audio range is too large".to_string())?;
            let output_end = output_start
                .checked_add(count)
                .ok_or_else(|| "edited audio duration is too large".to_string())?;
            let bound = SampleRange {
                source_start,
                source_end,
                output_start,
                output_end,
            };
            output_start = output_end;
            Ok(bound)
        })
        .collect()
}

fn seconds_to_samples(seconds: f64, rate: u32) -> Result<i64, String> {
    let value = seconds * rate as f64;
    if !value.is_finite() || value < 0.0 || value > i64::MAX as f64 {
        return Err("edited audio time is outside the supported range".into());
    }
    Ok(value.round() as i64)
}

fn gain_multiplier(gain_db: f64, muted: bool) -> f32 {
    if muted {
        0.0
    } else {
        10.0f64.powf(gain_db / 20.0) as f32
    }
}

#[cfg(test)]
mod tests {
    use super::{
        gain_multiplier, pcm_allocation_bytes, range_sample_bounds, MAX_EDITED_PCM_BYTES_PER_TRACK,
    };
    use crate::edit_model::TimeRange;

    #[test]
    fn range_samples_concatenate_without_a_gap() {
        let bounds = range_sample_bounds(
            &[
                TimeRange {
                    start: 1.0,
                    end: 1.5,
                },
                TimeRange {
                    start: 3.0,
                    end: 3.25,
                },
            ],
            48_000,
        )
        .unwrap();
        assert_eq!(bounds[0].source_start, 48_000);
        assert_eq!(bounds[0].output_end, 24_000);
        assert_eq!(bounds[1].output_start, 24_000);
        assert_eq!(bounds[1].output_end, 36_000);
    }

    #[test]
    fn mute_and_decibel_gain_are_linear() {
        assert_eq!(gain_multiplier(12.0, true), 0.0);
        assert!((gain_multiplier(-6.0, false) - 0.501_187_2).abs() < 0.000_001);
        assert!((gain_multiplier(12.0, false) - 3.981_071_7).abs() < 0.000_001);
    }

    #[test]
    fn pcm_allocation_is_bounded_and_overflow_checked() {
        assert_eq!(pcm_allocation_bytes(2, 48_000 * 60), Some(23_040_000));
        assert!(pcm_allocation_bytes(usize::MAX, 2).is_none());
        assert!(pcm_allocation_bytes(2, 48_000 * 700).unwrap() > MAX_EDITED_PCM_BYTES_PER_TRACK);
    }
}
