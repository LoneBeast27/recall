//! Read-only editor media preparation: cacheable PCM preview stems and peaks.
use crate::edit_model::{AudioTrackInfo, ClipInfo};
use ff::Rescale;
use ffmpeg_next as ff;
use std::fs::{self, File};
use std::io::{BufWriter, Seek, SeekFrom, Write};
use std::path::Path;
use std::time::UNIX_EPOCH;

const PREVIEW_RATE: u32 = 48_000;
const WAVE_BINS: usize = 1024;
const CACHE_VERSION: u32 = 4;

pub fn prepare_clip(input: &str, cache_dir: &str) -> Result<ClipInfo, String> {
    ff::init().map_err(|e| format!("FFmpeg initialization failed: {e}"))?;
    let source = fs::canonicalize(input).map_err(|e| format!("resolve input {input}: {e}"))?;
    let metadata = source
        .metadata()
        .map_err(|e| format!("stat {}: {e}", source.display()))?;
    if !metadata.is_file() {
        return Err(format!("input is not a file: {}", source.display()));
    }
    let modified_ms = metadata
        .modified()
        .unwrap_or(UNIX_EPOCH)
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(u64::MAX as u128) as u64;
    fs::create_dir_all(cache_dir).map_err(|e| format!("create cache directory: {e}"))?;
    let root = fs::canonicalize(cache_dir).map_err(|e| format!("resolve cache directory: {e}"))?;
    let key = cache_key(&source, metadata.len(), modified_ms);
    let cache = root.join(&key);
    let manifest = cache.join("clip.json");
    if let Ok(text) = fs::read_to_string(&manifest) {
        if let Ok(info) = serde_json::from_str::<ClipInfo>(&text) {
            if info.source_bytes == metadata.len()
                && info.source_modified_ms == modified_ms
                && info.path == source.to_string_lossy()
                && valid_cache(&info)
            {
                return Ok(info);
            }
        }
    }
    if cache.exists() {
        return Err(format!(
            "editor cache entry is incomplete or invalid: {}",
            cache.display()
        ));
    }
    let work = create_work_dir(&root, &key)?;
    let result = prepare_uncached(&source, metadata.len(), modified_ms, &work);
    match result {
        Ok(mut info) => {
            for track in &mut info.audio {
                track.preview_path = cache
                    .join(Path::new(&track.preview_path).file_name().unwrap())
                    .to_string_lossy()
                    .into_owned();
            }
            fs::write(
                work.join("clip.json"),
                serde_json::to_vec(&info).map_err(|e| e.to_string())?,
            )
            .map_err(|e| format!("write cache manifest: {e}"))?;
            fs::rename(&work, &cache).map_err(|e| format!("publish cache: {e}"))?;
            Ok(info)
        }
        Err(e) => {
            let _ = fs::remove_dir_all(&work);
            Err(e)
        }
    }
}

fn prepare_uncached(
    source: &Path,
    bytes: u64,
    modified_ms: u64,
    out: &Path,
) -> Result<ClipInfo, String> {
    let input = ff::format::input(source).map_err(|e| format!("open {}: {e}", source.display()))?;
    let video = input
        .streams()
        .best(ff::media::Type::Video)
        .ok_or("input has no video stream")?;
    let video_index = video.index();
    let origin_us = timeline_origin_us(&input, video_index);
    let duration = timeline_duration_us(&input, origin_us) as f64 / ff::ffi::AV_TIME_BASE as f64;
    let fps_r = video.avg_frame_rate();
    let video_fps = if fps_r.denominator() != 0 {
        fps_r.numerator() as f64 / fps_r.denominator() as f64
    } else {
        0.0
    };
    let video_decoder = ff::codec::context::Context::from_parameters(video.parameters())
        .map_err(|e| e.to_string())?
        .decoder()
        .video()
        .map_err(|e| e.to_string())?;
    let (width, height) = (video_decoder.width(), video_decoder.height());
    let tracks: Vec<TrackSpec> = input
        .streams()
        .filter(|s| s.parameters().medium() == ff::media::Type::Audio)
        .map(|s| {
            let title = s
                .metadata()
                .get("title")
                .map(str::to_owned)
                .or_else(|| s.metadata().get("language").map(str::to_owned))
                .unwrap_or_else(|| format!("Audio {}", s.index() + 1));
            TrackSpec {
                index: s.index(),
                name: title,
                time_base: s.time_base(),
                params: s.parameters(),
            }
        })
        .collect();
    let mut audio = Vec::with_capacity(tracks.len());
    for (ordinal, track) in tracks.iter().enumerate() {
        let file = format!("audio-{ordinal}.wav");
        let info = decode_track(source, track, duration, origin_us, &out.join(&file))?;
        audio.push(AudioTrackInfo {
            stream_index: track.index,
            name: track.name.clone(),
            channels: info.channels,
            sample_rate: PREVIEW_RATE,
            preview_path: file,
            waveform: info.waveform,
        });
    }
    let _ = video_index; // kept as an explicit validation that a video stream was selected.
    Ok(ClipInfo {
        path: source.to_string_lossy().into_owned(),
        duration,
        width,
        height,
        video_fps,
        source_bytes: bytes,
        source_modified_ms: modified_ms,
        audio,
    })
}

#[derive(Clone)]
struct TrackSpec {
    index: usize,
    name: String,
    time_base: ff::Rational,
    params: ff::codec::Parameters,
}
struct Decoded {
    channels: u16,
    waveform: Vec<f32>,
}

fn decode_track(
    source: &Path,
    track: &TrackSpec,
    duration: f64,
    origin_us: i64,
    wav: &Path,
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

fn valid_cache(info: &ClipInfo) -> bool {
    info.duration.is_finite()
        && info.duration >= 0.0
        && info
            .audio
            .iter()
            .all(|a| a.waveform.len() == WAVE_BINS && Path::new(&a.preview_path).is_file())
}
fn timeline_origin_us(input: &ff::format::context::Input, video_index: usize) -> i64 {
    let mut origin = None;
    for stream in input.streams() {
        let medium = stream.parameters().medium();
        if stream.index() != video_index && medium != ff::media::Type::Audio {
            continue;
        }
        let start = stream.start_time();
        if start != ff::ffi::AV_NOPTS_VALUE {
            let value = start.rescale(stream.time_base(), ff::rescale::TIME_BASE);
            origin = Some(origin.map_or(value, |old: i64| old.min(value)));
        }
    }
    origin.unwrap_or(0)
}
fn timeline_duration_us(input: &ff::format::context::Input, origin_us: i64) -> i64 {
    let mut duration = input.duration().max(0);
    for stream in input.streams() {
        let medium = stream.parameters().medium();
        if medium != ff::media::Type::Video && medium != ff::media::Type::Audio {
            continue;
        }
        if stream.duration() <= 0 || stream.duration() == ff::ffi::AV_NOPTS_VALUE {
            continue;
        }
        let start = if stream.start_time() == ff::ffi::AV_NOPTS_VALUE {
            origin_us.rescale(ff::rescale::TIME_BASE, stream.time_base())
        } else {
            stream.start_time()
        };
        duration = duration.max(
            start
                .saturating_add(stream.duration())
                .rescale(stream.time_base(), ff::rescale::TIME_BASE)
                .saturating_sub(origin_us),
        );
    }
    duration
}
fn create_work_dir(root: &Path, key: &str) -> Result<std::path::PathBuf, String> {
    for attempt in 0..32u32 {
        let candidate = root.join(format!(".{key}.{}-{attempt}", std::process::id()));
        match fs::create_dir(&candidate) {
            Ok(()) => return Ok(candidate),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(format!("create cache work directory: {e}")),
        }
    }
    Err("could not allocate an editor cache work directory".into())
}
fn cache_key(path: &Path, bytes: u64, modified_ms: u64) -> String {
    let mut h = 0xcbf29ce484222325u64;
    for b in path
        .to_string_lossy()
        .bytes()
        .chain(CACHE_VERSION.to_le_bytes())
        .chain(bytes.to_le_bytes())
        .chain(modified_ms.to_le_bytes())
    {
        h ^= b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    format!("{h:016x}")
}
