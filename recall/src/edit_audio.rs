//! Decode, edit, and AAC-encode the audio streams used by edited exports.

use std::path::Path;

use ff::Rescale;
use ffmpeg_next as ff;

use crate::edit_model::{AudioSetting, EditPlan, TimeRange};
use crate::export::edit_pcm::{gain_multiplier, EditedPcm};

pub(crate) struct EncodedAudioTrack {
    pub(crate) source_index: usize,
    pub(crate) rate: u32,
    pub(crate) encoder: ff::encoder::Audio,
    pub(crate) packets: Vec<ff::Packet>,
}

/// Edit every audio stream of `input` (in stream order) per `plan`.
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
    encode_selected_audio(input, origin_us, plan, &indices)
}

/// Edit only the listed audio streams (each range time-stretched by its speed).
pub(crate) fn encode_selected_audio(
    input: &Path,
    origin_us: i64,
    plan: &EditPlan,
    indices: &[usize],
) -> Result<Vec<EncodedAudioTrack>, String> {
    indices
        .iter()
        .copied()
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
    let gain = setting.map_or(1.0, |value| gain_multiplier(value.gain_db, value.muted));
    let mut pcm = EditedPcm::new(ranges, rate, channels, gain, stream_index)?;
    let origin_samples = origin_us.rescale(ff::rescale::TIME_BASE, ff::Rational(1, rate as i32));
    // Where the previous frame ended; the first frame goes to its timestamp.
    let mut next_source_sample: Option<i64> = None;
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
    flush_resampler(&mut resampler, &mut pcm, &mut next_source_sample, stream_index)?;

    encode_pcm(stream_index, rate, layout, pcm.finish(rate, layout)?)
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
