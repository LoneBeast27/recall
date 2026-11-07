//! One audio track -> its editor preview stem: decode, resample to 48 kHz
//! packed i16 (samples kept back to back; re-seeded only at real timestamp gaps,
//! see `audio_clock`), write the FLAC stem (`flac.rs`), and fold the waveform peaks.

use std::path::Path;

use ff::Rescale;
use ffmpeg_next as ff;

use super::flac::FlacWriter;
use crate::export::bad_audio::BadPackets;
use super::{Decoded, TrackSpec, PREVIEW_RATE, WAVE_BINS};
use crate::export::keyframes::KeyframeScan;

pub(super) fn decode_track(
    source: &Path,
    track: &TrackSpec,
    duration: f64,
    origin_us: i64,
    stem: &Path,
    mut keyframes: Option<&mut KeyframeScan>,
) -> Result<Decoded, String> {
    // Each pass starts at the source: packet iterators consume their input context.
    let mut input =
        ff::format::input(source).map_err(|e| format!("reopen {}: {e}", source.display()))?;
    let mut decoder = ff::codec::context::Context::from_parameters(track.params.clone())
        .map_err(|e| format!("create audio decoder: {e}"))?
        .decoder()
        .audio()
        .map_err(|e| format!("open audio decoder: {e}"))?;
    let channels = decoder.channels().clamp(1, 2);
    let layout = if channels == 1 {
        ff::ChannelLayout::MONO
    } else {
        ff::ChannelLayout::STEREO
    };
    let mut resampler = ff::software::resampling::Context::get(
        decoder.format(),
        decoder.channel_layout(),
        decoder.rate(),
        ff::format::Sample::I16(ff::format::sample::Type::Packed),
        layout,
        PREVIEW_RATE,
    )
    .map_err(|e| format!("create audio resampler: {e}"))?;
    let total_samples = (duration.max(0.0) * PREVIEW_RATE as f64).ceil() as u64;
    let mut writer = FlacWriter::create(stem, channels)?;
    let mut peaks = vec![0.0f32; WAVE_BINS];
    let mut cursor = 0u64;
    // Where the previous frame ended (source samples); the first frame goes to its timestamp.
    let mut next_source_sample: Option<i64> = None;
    let mut frame = ff::frame::Audio::empty();
    let mut bad = BadPackets::default();
    for (stream, packet) in input.packets() {
        if let Some(scan) = keyframes.as_deref_mut() {
            scan.observe(stream.index(), &packet);
        }
        if stream.index() != track.index {
            continue;
        }
        bad.send(&mut decoder, &packet, "decode audio packet")?;
        drain(
            &mut decoder,
            &mut resampler,
            &mut frame,
            track.time_base,
            origin_us,
            &mut writer,
            &mut cursor,
            &mut next_source_sample,
            total_samples,
            &mut peaks,
            channels,
            &mut bad,
        )?;
    }
    decoder
        .send_eof()
        .map_err(|e| format!("finish audio decoder: {e}"))?;
    drain(
        &mut decoder,
        &mut resampler,
        &mut frame,
        track.time_base,
        origin_us,
        &mut writer,
        &mut cursor,
        &mut next_source_sample,
        total_samples,
        &mut peaks,
        channels,
        &mut bad,
    )?;
    bad.report(&format!("audio stream {}", track.index));
    flush_resampler(
        &mut resampler,
        &mut writer,
        &mut cursor,
        total_samples,
        &mut peaks,
        channels,
    )?;
    // Make the stem as long as the source timeline; absent samples are intentional silence.
    if cursor < total_samples {
        writer.silence(total_samples - cursor)?;
    }
    writer.finish()?;
    Ok(Decoded {
        channels,
        waveform: peaks,
    })
}

#[allow(clippy::too_many_arguments)]
fn drain(
    decoder: &mut ff::decoder::Audio,
    resampler: &mut ff::software::resampling::Context,
    frame: &mut ff::frame::Audio,
    tb: ff::Rational,
    origin_us: i64,
    writer: &mut FlacWriter,
    cursor: &mut u64,
    next_source_sample: &mut Option<i64>,
    total: u64,
    peaks: &mut [f32],
    channels: u16,
    bad: &mut BadPackets,
) -> Result<(), String> {
    loop {
        match decoder.receive_frame(frame) {
            Ok(()) => {
                let input_rate = frame.rate().max(1);
                let origin_samples =
                    origin_us.rescale(ff::rescale::TIME_BASE, ff::Rational(1, input_rate as i32));
                let stamped = frame.pts().map(|pts| {
                    pts.rescale(tb, ff::Rational(1, input_rate as i32))
                        .saturating_sub(origin_samples)
                });
                // A skipped packet's time stays silent: this frame goes after it, placed by
                // `append_at` (which pads), not closed up against the previous one.
                let gap = bad.take_gap().rescale(tb, ff::Rational(1, input_rate as i32));
                let skipped = gap > 0 && next_source_sample.is_some();
                let expected = next_source_sample.map(|next| next.saturating_add(gap));
                let (source_start, reseeded) = crate::audio_clock::frame_start(
                    expected,
                    stamped,
                    crate::audio_clock::drift_tolerance(input_rate),
                );
                let discontinuity = reseeded || skipped;
                if discontinuity {
                    flush_resampler(resampler, writer, cursor, total, peaks, channels)?;
                    let input = *resampler.input();
                    let output = *resampler.output();
                    *resampler = ff::software::resampling::Context::get(
                        input.format,
                        input.channel_layout,
                        input.rate,
                        output.format,
                        output.channel_layout,
                        output.rate,
                    )
                    .map_err(|e| format!("reset audio resampler at timestamp gap: {e}"))?;
                }
                let cap = ((frame.samples() as u64 * PREVIEW_RATE as u64 + input_rate as u64 - 1)
                    / input_rate as u64
                    + 64) as usize;
                let mut converted = ff::frame::Audio::new(
                    ff::format::Sample::I16(ff::format::sample::Type::Packed),
                    cap,
                    if channels == 1 {
                        ff::ChannelLayout::MONO
                    } else {
                        ff::ChannelLayout::STEREO
                    },
                );
                converted.set_rate(PREVIEW_RATE);
                resampler
                    .run(frame, &mut converted)
                    .map_err(|e| format!("resample audio: {e}"))?;
                // `frame::Audio::plane::<i16>` is per-frame for packed samples; use
                // the byte plane so both mono and stereo interleaved samples survive.
                let byte_count = converted
                    .samples()
                    .saturating_mul(channels as usize)
                    .saturating_mul(2);
                let samples: Vec<i16> = converted.data(0)[..byte_count]
                    .chunks_exact(2)
                    .map(|b| i16::from_le_bytes([b[0], b[1]]))
                    .collect();
                if discontinuity {
                    let start = source_start.max(0).rescale(
                        ff::Rational(1, input_rate as i32),
                        ff::Rational(1, PREVIEW_RATE as i32),
                    ) as u64;
                    append_at(&samples, start, writer, cursor, total, peaks, channels)?;
                } else {
                    append_samples(&samples, writer, cursor, total, peaks, channels)?;
                }
                *next_source_sample = Some(source_start.saturating_add(frame.samples() as i64));
            }
            Err(error) if crate::export::receive_is_drained(&error) => break,
            Err(error) => {
                bad.skip(error, "receive decoded audio frame")?;
                break;
            }
        }
    }
    Ok(())
}

fn flush_resampler(
    resampler: &mut ff::software::resampling::Context,
    writer: &mut FlacWriter,
    cursor: &mut u64,
    total: u64,
    peaks: &mut [f32],
    channels: u16,
) -> Result<(), String> {
    loop {
        let mut frame = ff::frame::Audio::new(
            ff::format::Sample::I16(ff::format::sample::Type::Packed),
            2048,
            if channels == 1 {
                ff::ChannelLayout::MONO
            } else {
                ff::ChannelLayout::STEREO
            },
        );
        frame.set_rate(PREVIEW_RATE);
        let _delay = resampler
            .flush(&mut frame)
            .map_err(|e| format!("flush audio resampler: {e}"))?;
        let count = frame
            .samples()
            .saturating_mul(channels as usize)
            .saturating_mul(2);
        let samples: Vec<i16> = frame.data(0)[..count]
            .chunks_exact(2)
            .map(|b| i16::from_le_bytes([b[0], b[1]]))
            .collect();
        append_samples(&samples, writer, cursor, total, peaks, channels)?;
        if samples.is_empty() {
            break;
        }
    }
    Ok(())
}

fn append_at(
    samples: &[i16],
    start: u64,
    writer: &mut FlacWriter,
    cursor: &mut u64,
    total: u64,
    peaks: &mut [f32],
    channels: u16,
) -> Result<(), String> {
    // Resampling 44.1 kHz into 48 kHz can round adjacent frame boundaries by
    // a sample or two. Larger deltas are real source-time discontinuities.
    const ROUNDING_TOLERANCE: u64 = 2;
    if start > cursor.saturating_add(ROUNDING_TOLERANCE) {
        let gap = (start - *cursor).min(total.saturating_sub(*cursor));
        writer.silence(gap)?;
        *cursor += gap;
    }
    let skip_frames = if *cursor > start.saturating_add(ROUNDING_TOLERANCE) {
        (*cursor - start) as usize
    } else {
        0
    };
    let skip_samples = skip_frames
        .saturating_mul(channels as usize)
        .min(samples.len());
    append_samples(
        &samples[skip_samples..],
        writer,
        cursor,
        total,
        peaks,
        channels,
    )
}

fn append_samples(
    samples: &[i16],
    writer: &mut FlacWriter,
    cursor: &mut u64,
    total: u64,
    peaks: &mut [f32],
    channels: u16,
) -> Result<(), String> {
    let remaining = total.saturating_sub(*cursor) as usize * channels as usize;
    let usable = &samples[..samples.len().min(remaining)];
    writer.samples(usable)?;
    for (i, sample) in usable.iter().enumerate() {
        let pos = *cursor + (i / channels as usize) as u64;
        let bin =
            ((pos.saturating_mul(WAVE_BINS as u64) / total.max(1)) as usize).min(WAVE_BINS - 1);
        peaks[bin] = peaks[bin].max((*sample as i32).unsigned_abs() as f32 / 32768.0);
    }
    *cursor += (usable.len() / channels as usize) as u64;
    Ok(())
}
