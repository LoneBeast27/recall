//! Decode, edit, and AAC-encode the audio streams used by edited exports.

use std::path::Path;

use ff::Rescale;
use ffmpeg_next as ff;

use crate::edit_model::{AudioOutput, AudioSetting, EditPlan, TimeRange};
use crate::export::bad_audio::BadPackets;
use crate::export::edit_pcm::{gain_multiplier, EditedPcm};
use crate::export::timeline;

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
                let (rate, layout, pcm) = decode_track(input, origin_us, &plan.ranges, *index, setting(*index), None)?;
                encode_pcm(*index, false, rate, layout, pcm)
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
    let mut sum: Vec<Vec<f32>> = Vec::new();
    for &index in indices {
        let setting = plan.audio.iter().find(|s| s.stream_index == index);
        let (_, _, pcm) = decode_track(input, origin_us, &plan.ranges, index, setting, Some(target))?;
        add_into(&mut sum, &pcm);
    }
    // Unity-gain sum; two loud sources can pass full scale, so a limiter, never a hard clip.
    crate::export::mix_limit::limit_mix(&mut sum, target.0);
    encode_pcm(first, true, target.0, target.1, sum)
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

/// One stream's kept ranges as planar f32 (gain applied): its own rate and layout, or `target`.
fn decode_track(
    input: &Path,
    origin_us: i64,
    ranges: &[TimeRange],
    stream_index: usize,
    setting: Option<&AudioSetting>,
    target: Option<(u32, ff::ChannelLayout)>,
) -> Result<(u32, ff::ChannelLayout, Vec<Vec<f32>>), String> {
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
    let (rate, layout) = target.unwrap_or((rate, layout));
    let channels = layout.channels().max(1) as usize;
    let sample_format = ff::format::Sample::F32(ff::format::sample::Type::Planar);
    let gain = setting.map_or(1.0, |value| gain_multiplier(value.gain_db, value.muted));
    let mut pcm = EditedPcm::new(ranges, rate, channels, gain, stream_index)?;
    let origin_samples = origin_us.rescale(ff::rescale::TIME_BASE, ff::Rational(1, rate as i32));
    // Where the previous frame ended; the first frame goes to its timestamp.
    let mut next_source_sample: Option<i64> = None;
    let mut resampler: Option<ff::software::resampling::Context> = None;
    // Read only around the kept ranges; the first frame after a seek lands at
    // its own timestamp, and the margin keeps decoder warm-up out of the ranges.
    let trim = timeline::trim_decode() && !ranges.is_empty();
    let stop_us = if trim {
        let first = timeline::seconds_to_us(ranges[0].start)?;
        timeline::seek_near(&mut ictx, origin_us, first);
        let last = ranges.iter().map(|r| r.end).fold(0.0, f64::max);
        timeline::seconds_to_us(last)?.saturating_add(timeline::SEEK_MARGIN_US)
    } else {
        i64::MAX
    };

    let what = format!("decode audio packet for stream {stream_index}");
    let mut bad = BadPackets::default();
    for (stream, packet) in ictx.packets() {
        if stream.index() != stream_index {
            continue;
        }
        let packet_us = packet.pts().or(packet.dts()).map(|pts| {
            pts.rescale(stream_tb, ff::rescale::TIME_BASE).saturating_sub(origin_us)
        });
        if packet_us.is_some_and(|us| us > stop_us) {
            break;
        }
        bad.send(&mut decoder, &packet, &what)?;
        // A skipped packet's time stays silent (`bad_audio`): the next frame goes after it.
        let gap = bad.take_gap().rescale(stream_tb, ff::Rational(1, rate as i32));
        next_source_sample = next_source_sample.map(|next| next.saturating_add(gap));
        receive_audio_frames(
            &mut decoder,
            &mut resampler,
            &mut pcm,
            layout,
            rate,
            sample_format,
            stream_tb,
            origin_samples,
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
        layout,
        rate,
        sample_format,
        stream_tb,
        origin_samples,
        &mut next_source_sample,
        stream_index,
    )?;
    bad.report(&format!("audio stream {stream_index}"));
    flush_resampler(&mut resampler, &mut pcm, &mut next_source_sample, stream_index)?;

    Ok((rate, layout, pcm.finish(rate, layout)?))
}

#[allow(clippy::too_many_arguments)]
fn receive_audio_frames(
    decoder: &mut ff::decoder::Audio,
    resampler: &mut Option<ff::software::resampling::Context>,
    pcm: &mut EditedPcm,
    output_layout: ff::ChannelLayout,
    output_rate: u32,
    output_format: ff::format::Sample,
    stream_tb: ff::Rational,
    origin_samples: i64,
    next_source_sample: &mut Option<i64>,
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
                    flush_resampler(resampler, pcm, next_source_sample, stream_index)?;
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
                let stamped = decoded.timestamp().or(decoded.pts()).map(|pts| {
                    pts.rescale(stream_tb, ff::Rational(1, output_rate as i32))
                        .saturating_sub(origin_samples)
                });
                // Back to back unless the stamps show a real gap (see `audio_clock`).
                let (source_start, _) = crate::audio_clock::frame_start(
                    *next_source_sample,
                    stamped,
                    crate::audio_clock::drift_tolerance(output_rate),
                );
                pcm.place(&converted, source_start);
                *next_source_sample = Some(source_start.saturating_add(converted.samples() as i64));
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
    pcm: &mut EditedPcm,
    next_source_sample: &mut Option<i64>,
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
        let at = next_source_sample.unwrap_or(0);
        pcm.place(&delayed, at);
        *next_source_sample = Some(at.saturating_add(produced as i64));
    }
    Ok(())
}

fn encode_pcm(
    source_index: usize,
    mixed: bool,
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
        mixed,
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
