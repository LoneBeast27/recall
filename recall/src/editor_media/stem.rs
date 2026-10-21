//! One audio track -> its editor preview stem: decode, resample to 48 kHz
//! packed i16 (re-seeded at source timestamp gaps), write the WAV, and fold the
//! waveform peaks.

use std::fs::File;
use std::io::{BufWriter, Seek, SeekFrom, Write};
use std::path::Path;

use ff::Rescale;
use ffmpeg_next as ff;

use super::{Decoded, TrackSpec, PREVIEW_RATE, WAVE_BINS};
use crate::export::keyframes::KeyframeScan;

pub(super) fn decode_track(
    source: &Path,
    track: &TrackSpec,
    duration: f64,
    origin_us: i64,
    wav: &Path,
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
    if total_samples
        .saturating_mul(channels as u64)
        .saturating_mul(2)
        > u32::MAX as u64 - 36
    {
        return Err("editor preview stem exceeds the RIFF 4 GiB limit".into());
    }
    let mut writer = WavWriter::create(wav, channels)?;
    let mut peaks = vec![0.0f32; WAVE_BINS];
    let mut cursor = 0u64;
    let mut next_source_sample = Some(0i64);
    let mut frame = ff::frame::Audio::empty();
    for (stream, packet) in input.packets() {
        if let Some(scan) = keyframes.as_deref_mut() {
            scan.observe(stream.index(), &packet);
        }
        if stream.index() != track.index {
            continue;
        }
        decoder
            .send_packet(&packet)
            .map_err(|e| format!("decode audio packet: {e}"))?;
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
    )?;
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
    writer: &mut WavWriter,
    cursor: &mut u64,
    next_source_sample: &mut Option<i64>,
    total: u64,
    peaks: &mut [f32],
    channels: u16,
) -> Result<(), String> {
    loop {
        match decoder.receive_frame(frame) {
            Ok(()) => {
                let input_rate = frame.rate().max(1);
                let origin_samples =
                    origin_us.rescale(ff::rescale::TIME_BASE, ff::Rational(1, input_rate as i32));
                let source_start = frame
                    .pts()
                    .map(|pts| {
                        pts.rescale(tb, ff::Rational(1, input_rate as i32))
                            .saturating_sub(origin_samples)
                    })
                    .unwrap_or_else(|| next_source_sample.unwrap_or(0));
                let delta = source_start.saturating_sub(next_source_sample.unwrap_or(source_start));
                let delta_preview = delta.rescale(
                    ff::Rational(1, input_rate as i32),
                    ff::Rational(1, PREVIEW_RATE as i32),
                );
                let discontinuity = delta_preview.unsigned_abs() > 2;
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
            Err(error) => return Err(format!("receive decoded audio frame: {error}")),
        }
    }
    Ok(())
}

fn flush_resampler(
    resampler: &mut ff::software::resampling::Context,
    writer: &mut WavWriter,
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
    writer: &mut WavWriter,
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
    writer: &mut WavWriter,
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

struct WavWriter {
    out: BufWriter<File>,
    bytes: u64,
    channels: u16,
}
impl WavWriter {
    fn create(path: &Path, channels: u16) -> Result<Self, String> {
        let mut out = BufWriter::new(
            File::create(path).map_err(|e| format!("create stem {}: {e}", path.display()))?,
        );
        out.write_all(&[0; 44]).map_err(|e| e.to_string())?;
        Ok(Self {
            out,
            bytes: 0,
            channels,
        })
    }
    fn samples(&mut self, samples: &[i16]) -> Result<(), String> {
        let added = (samples.len() * 2) as u64;
        if self.bytes.saturating_add(added) > u32::MAX as u64 - 36 {
            return Err("editor preview stem exceeds the RIFF 4 GiB limit".into());
        }
        for s in samples {
            self.out
                .write_all(&s.to_le_bytes())
                .map_err(|e| e.to_string())?;
        }
        self.bytes += added;
        Ok(())
    }
    fn silence(&mut self, frames: u64) -> Result<(), String> {
        const ZERO: [u8; 8192] = [0; 8192];
        let mut bytes = frames
            .saturating_mul(self.channels as u64)
            .saturating_mul(2);
        if self.bytes.saturating_add(bytes) > u32::MAX as u64 - 36 {
            return Err("editor preview stem exceeds the RIFF 4 GiB limit".into());
        }
        while bytes > 0 {
            let n = bytes.min(ZERO.len() as u64) as usize;
            self.out.write_all(&ZERO[..n]).map_err(|e| e.to_string())?;
            bytes -= n as u64;
            self.bytes += n as u64;
        }
        Ok(())
    }
    fn finish(mut self) -> Result<(), String> {
        self.out.flush().map_err(|e| e.to_string())?;
        let mut file = self.out.into_inner().map_err(|e| e.to_string())?;
        file.seek(SeekFrom::Start(0)).map_err(|e| e.to_string())?;
        let rate = PREVIEW_RATE * self.channels as u32 * 2;
        let mut h = Vec::with_capacity(44);
        h.extend_from_slice(b"RIFF");
        h.extend_from_slice(&(36 + self.bytes as u32).to_le_bytes());
        h.extend_from_slice(b"WAVEfmt ");
        h.extend_from_slice(&16u32.to_le_bytes());
        h.extend_from_slice(&1u16.to_le_bytes());
        h.extend_from_slice(&self.channels.to_le_bytes());
        h.extend_from_slice(&PREVIEW_RATE.to_le_bytes());
        h.extend_from_slice(&rate.to_le_bytes());
        h.extend_from_slice(&(self.channels * 2).to_le_bytes());
        h.extend_from_slice(&16u16.to_le_bytes());
        h.extend_from_slice(b"data");
        h.extend_from_slice(&(self.bytes as u32).to_le_bytes());
        file.write_all(&h).map_err(|e| e.to_string())
    }
}
