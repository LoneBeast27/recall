//! Editor visuals (poster + scene-picked filmstrip) in the SSD frame tier
//! (`<cache>/visuals/<identity>/`, shared with hover-scrub frames). Keyed by the original
//! clip, extracted from whichever copy is cheapest to read (the SSD video copy after an open).
//!
//! The filmstrip is one ffmpeg pass over the clip's keyframes: each one becomes a tiny sample
//! for [`scene_strip::scores`] and a 320 px JPEG; the [`scene_strip::MAX_FRAMES`] kept ones
//! are listed in `strip.json` with their times, scores and the picture's aspect ratio.
use std::path::{Path, PathBuf};
use std::process::Command;

use serde::{Deserialize, Serialize};

use crate::backend::VisualAssets;
use super::scene_strip::{self, SAMPLE_BYTES, SAMPLE_H, SAMPLE_W, StripFrame};
use crate::services::storage::{self, Seek};

const WIDTH: u32 = 320;
const MANIFEST: &str = "strip.json";
/// Frames taken evenly when the keyframe scan fails.
const FALLBACK_FRAMES: usize = 16;

#[derive(Serialize, Deserialize)]
struct Manifest {
    version: u32,
    aspect: f32,
    frames: Vec<(String, f64, f32)>,
}

/// The poster and filmstrip already cached for `original`, extracting nothing.
pub(crate) fn cached(original: &Path) -> VisualAssets {
    let Ok((id, _, _)) = storage::identity_of(original) else { return VisualAssets::default() };
    let directory = storage::visuals_dir(&storage::cache_root(), &id);
    let poster = Some(directory.join("poster.jpg")).filter(|p| storage::valid_file(p)).map(|p| p.to_string_lossy().into_owned());
    let (filmstrip, aspect) = read_manifest(&directory).unwrap_or_default();
    VisualAssets { poster, filmstrip, aspect }
}

fn read_manifest(directory: &Path) -> Option<(Vec<StripFrame>, f32)> {
    let manifest: Manifest = serde_json::from_slice(&std::fs::read(directory.join(MANIFEST)).ok()?).ok()?;
    let frames: Option<Vec<StripFrame>> = manifest
        .frames
        .into_iter()
        .map(|(file, seconds, score)| {
            let path = directory.join(file);
            storage::valid_file(&path).then(|| StripFrame { path: path.to_string_lossy().into_owned(), seconds, score })
        })
        .collect();
    frames.filter(|f| !f.is_empty()).map(|f| (f, manifest.aspect))
}

fn write_manifest(directory: &Path, frames: &[StripFrame], aspect: f32) -> Result<(), String> {
    let manifest = Manifest {
        version: 1,
        aspect,
        frames: frames
            .iter()
            .map(|f| (Path::new(&f.path).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default(), f.seconds, f.score))
            .collect(),
    };
    let target = directory.join(MANIFEST);
    let temporary = storage::temp_sibling(&target);
    std::fs::write(&temporary, serde_json::to_vec(&manifest).map_err(|e| e.to_string())?).map_err(|e| format!("write strip list: {e}"))?;
    std::fs::rename(&temporary, &target).map_err(|e| {
        let _ = std::fs::remove_file(&temporary);
        format!("publish strip list: {e}")
    })
}

/// The successful frame paths are useful even if a later representative seek fails.
pub(crate) fn visuals_best_effort(original: &Path, read_from: &Path, source_duration: f64) -> (VisualAssets, Option<String>) {
    let id = match storage::identity_of(original) {
        Ok((id, _, _)) => id,
        Err(error) => return (VisualAssets::default(), Some(format!("Visual previews: {error}"))),
    };
    let directory = storage::visuals_dir(&storage::cache_root(), &id);
    if let Err(error) = std::fs::create_dir_all(&directory) {
        return (VisualAssets::default(), Some(format!("Visual previews: create cache: {error}")));
    }
    let lowered = storage::drive_kind(read_from).is_hdd();
    let duration = if source_duration.is_finite() && source_duration > 0.0 {
        source_duration
    } else {
        match storage::probe_duration(read_from, lowered) {
            Ok(value) => value,
            Err(error) => return (VisualAssets::default(), Some(error)),
        }
    };
    let frame = |label: &str, seconds: f64| -> Result<String, String> {
        let target = directory.join(format!("{label}.jpg"));
        if !storage::valid_file(&target) {
            storage::extract_frame(read_from, seconds, WIDTH, 4, &target, Seek::Exact, lowered)?;
        }
        Ok(target.to_string_lossy().into_owned())
    };
    let mut warnings = Vec::new();
    let poster = match frame("poster", duration * 0.05) {
        Ok(value) => Some(value),
        Err(error) => {
            warnings.push(format!("poster: {error}"));
            None
        }
    };
    let (filmstrip, aspect) = match read_manifest(&directory) {
        Some(found) => found,
        None => match scan_strip(read_from, &directory, duration, lowered) {
            Ok(found) => found,
            Err(scan_error) => {
                // Evenly spaced frames, as before the scene scan: still worth showing.
                let mut frames = Vec::with_capacity(FALLBACK_FRAMES);
                for index in 0..FALLBACK_FRAMES {
                    let seconds = duration * ((index as f64 + 0.5) / FALLBACK_FRAMES as f64);
                    match frame(&format!("strip-{index}"), seconds) {
                        Ok(path) => frames.push(StripFrame { path, seconds, score: 0.0 }),
                        Err(error) => warnings.push(format!("frame {}: {error}", index + 1)),
                    }
                }
                if frames.len() < FALLBACK_FRAMES {
                    warnings.push(format!("scene scan: {scan_error}"));
                } else {
                    let _ = write_manifest(&directory, &frames, 0.0);
                }
                (frames, 0.0)
            }
        },
    };
    let warning = if warnings.is_empty() { None } else { Some(format!("Visual previews: {}", warnings.join("; "))) };
    (VisualAssets { poster, filmstrip, aspect }, warning)
}

/// One pass over the keyframes (`-discard nokey`: only keyframe packets are read into the
/// decoder): tiny rgb24 samples to stdout, a 320 px JPEG each into a scratch folder, and the
/// times via `showinfo`. Keeps the [`scene_strip::keep`] frames as `strip-<i>.jpg`.
fn scan_strip(read_from: &Path, directory: &Path, duration: f64, lowered: bool) -> Result<(Vec<StripFrame>, f32), String> {
    let scratch = storage::temp_sibling(&directory.join("strip-scan"));
    std::fs::create_dir_all(&scratch).map_err(|e| format!("create scan folder: {e}"))?;
    let result = scan_into(read_from, directory, &scratch, duration, lowered);
    let _ = std::fs::remove_dir_all(&scratch);
    result
}

fn scan_into(read_from: &Path, directory: &Path, scratch: &Path, duration: f64, lowered: bool) -> Result<(Vec<StripFrame>, f32), String> {
    let graph = format!("[0:v:0]showinfo,split=2[a][b];[a]scale={SAMPLE_W}:{SAMPLE_H}:flags=area,format=rgb24[s];[b]scale={WIDTH}:-2[t]");
    let mut command = Command::new(storage::binary("RECALL_FFMPEG", "ffmpeg.exe")?);
    command
        .args(["-hide_banner", "-nostats", "-loglevel", "info", "-threads", "2", "-filter_threads", "1"])
        .args(["-discard", "nokey", "-skip_frame", "nokey", "-i"])
        .arg(read_from)
        .args(["-an", "-sn", "-dn", "-filter_complex", &graph])
        .args(["-map", "[s]", "-fps_mode", "passthrough", "-f", "rawvideo", "pipe:1"])
        .args(["-map", "[t]", "-fps_mode", "passthrough", "-q:v", "5"])
        .arg(scratch.join("k-%05d.jpg"));
    let output = storage::run_hidden(&mut command, lowered).map_err(|e| format!("start ffmpeg: {e}"))?;
    let log = String::from_utf8_lossy(&output.stderr);
    if !output.status.success() {
        let tail: Vec<&str> = log.lines().rev().take(3).collect();
        return Err(format!("ffmpeg scan: {}", tail.into_iter().rev().collect::<Vec<_>>().join(" ").trim().to_string()));
    }
    let (times, size) = scene_strip::parse_showinfo(&log);
    let count = output.stdout.len() / SAMPLE_BYTES;
    if count == 0 || count != times.len() {
        return Err(format!("ffmpeg scan: {count} samples for {} times", times.len()));
    }
    let samples: Vec<&[u8]> = output.stdout.chunks_exact(SAMPLE_BYTES).collect();
    let scores = scene_strip::scores(&samples);
    let times: Vec<f64> = times.into_iter().map(|t| t.clamp(0.0, duration)).collect();
    let mut frames = Vec::new();
    for (slot, index) in scene_strip::keep(&times, &scores, duration, scene_strip::MAX_FRAMES).into_iter().enumerate() {
        let from: PathBuf = scratch.join(format!("k-{:05}.jpg", index + 1));
        let target = directory.join(format!("strip-{slot}.jpg"));
        if !storage::valid_file(&from) {
            return Err(format!("ffmpeg scan: frame {} missing", index + 1));
        }
        std::fs::rename(&from, &target).map_err(|e| format!("keep frame: {e}"))?;
        frames.push(StripFrame { path: target.to_string_lossy().into_owned(), seconds: times[index], score: scores[index] });
    }
    let aspect = size.filter(|(w, h)| *w > 0 && *h > 0).map_or(0.0, |(w, h)| w as f32 / h as f32);
    write_manifest(directory, &frames, aspect)?;
    Ok((frames, aspect))
}
