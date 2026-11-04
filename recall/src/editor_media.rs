//! Read-only editor media preparation: cacheable PCM preview stems and peaks.
//! The per-track stem decode lives in `editor_media/stem.rs`, the FLAC writer in `editor_media/flac.rs`.
mod flac;
mod stem;
#[allow(unused_imports)]
pub(crate) use flac::read_stem;

use crate::edit_model::{AudioTrackInfo, ClipInfo};
use crate::export::keyframes::{scan_keyframes, KeyframeScan};
use crate::export::timeline::{timeline_duration_us, timeline_origin_us};
use ffmpeg_next as ff;
use std::fs;
use std::path::Path;
use std::time::UNIX_EPOCH;
use stem::decode_track;

const PREVIEW_RATE: u32 = 48_000;
const WAVE_BINS: usize = 1024;
// 5: stems no longer follow per-frame timestamp jitter (Moments clips played choppy).
/// 6: preview stems are FLAC (`audio-<n>.flac`), not WAV. Folders of older versions are
/// left behind and removed by the app once a clip is prepared again.
const CACHE_VERSION: u32 = 6;

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
        if let Ok(mut info) = serde_json::from_str::<ClipInfo>(&text) {
            if info.source_bytes == metadata.len()
                && info.source_modified_ms == modified_ms
                && info.path == source.to_string_lossy()
                && valid_cache(&info)
            {
                // Manifests written before keyframes existed: scan once, refresh the
                // track names (handler_name), keep the stems.
                if info.keyframes.is_empty() {
                    info.keyframes = scan_keyframes(&source)?;
                    refresh_track_names(&source, &mut info)?;
                    let _ = rewrite_manifest(&manifest, &info);
                }
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
            let title =
                track_name(&s.metadata()).unwrap_or_else(|| format!("Audio {}", s.index() + 1));
            TrackSpec {
                index: s.index(),
                name: title,
                time_base: s.time_base(),
                params: s.parameters(),
            }
        })
        .collect();
    let sources = audio_sources(&input);
    // Keyframes ride along the first audio pass; a clip without audio gets its own scan.
    let mut scan = KeyframeScan::for_input(&input);
    let mut audio = Vec::with_capacity(tracks.len());
    for (ordinal, track) in tracks.iter().enumerate() {
        let file = format!("audio-{ordinal}.flac");
        let keyframes = if ordinal == 0 { scan.as_mut() } else { None };
        let info = decode_track(source, track, duration, origin_us, &out.join(&file), keyframes)?;
        audio.push(AudioTrackInfo {
            stream_index: track.index,
            name: track.name.clone(),
            channels: info.channels,
            sample_rate: PREVIEW_RATE,
            preview_path: file,
            waveform: info.waveform,
            source: sources.get(ordinal).cloned().unwrap_or_default(),
        });
    }
    let _ = video_index; // kept as an explicit validation that a video stream was selected.
    let keyframes = match (scan, tracks.is_empty()) {
        (Some(scan), false) => scan.seconds(),
        _ => scan_keyframes(source)?,
    };
    Ok(ClipInfo {
        path: source.to_string_lossy().into_owned(),
        duration,
        width,
        height,
        video_fps,
        source_bytes: bytes,
        source_modified_ms: modified_ms,
        audio,
        keyframes,
    })
}

/// Track label from stream metadata. MP4 keeps no readable per-track `title`,
/// so Recall names tracks through `handler_name`; FFmpeg's placeholder
/// handler ("SoundHandler") and blanks are skipped.
/// Each audio track's source, as the recorder saved it, in audio-stream order (empty for
/// clips without the tag: older recordings and other apps' files).
pub(crate) fn audio_sources(input: &ff::format::context::Input) -> Vec<String> {
    crate::edit_model::parse_audio_sources(input.metadata().get(crate::edit_model::AUDIO_SOURCES_TAG))
}

pub(crate) fn track_name(metadata: &ff::DictionaryRef) -> Option<String> {
    let usable = |value: &&str| !value.trim().is_empty();
    metadata
        .get("title")
        .filter(usable)
        .or_else(|| {
            metadata
                .get("handler_name")
                .filter(usable)
                .filter(|value| value.trim() != "SoundHandler")
        })
        .or_else(|| metadata.get("language").filter(usable))
        .map(|value| value.trim().to_owned())
}

fn refresh_track_names(source: &Path, info: &mut ClipInfo) -> Result<(), String> {
    let input = ff::format::input(source).map_err(|e| format!("open {}: {e}", source.display()))?;
    for track in &mut info.audio {
        if let Some(name) = input.stream(track.stream_index).and_then(|s| track_name(&s.metadata())) {
            track.name = name;
        }
    }
    Ok(())
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

fn valid_cache(info: &ClipInfo) -> bool {
    info.duration.is_finite()
        && info.duration >= 0.0
        && info
            .audio
            .iter()
            .all(|a| a.waveform.len() == WAVE_BINS && Path::new(&a.preview_path).is_file())
}
/// Replace a published manifest atomically (temp file + rename).
fn rewrite_manifest(manifest: &Path, info: &ClipInfo) -> Result<(), String> {
    let temp = manifest.with_extension(format!("json.{}.tmp", std::process::id()));
    fs::write(&temp, serde_json::to_vec(info).map_err(|e| e.to_string())?)
        .map_err(|e| format!("write cache manifest: {e}"))?;
    fs::rename(&temp, manifest).map_err(|e| {
        let _ = fs::remove_file(&temp);
        format!("publish cache manifest: {e}")
    })
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
