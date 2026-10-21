//! Hover-scrub frames: 8 × 320 px JPEGs at (i+0.5)/8 of the clip, cached on the SSD at
//! `<cache>/visuals/<identity>/scrub-<i>.jpg` (the folder the editor's visuals share).
//!
//! Frames come from the SSD video copy when one exists; otherwise from the clip itself with
//! fast (keyframe) seeks and ffmpeg at very low I/O priority. Callers route HDD clips
//! through the disk lane (`worker.rs`).
use std::path::{Path, PathBuf};

use crate::services::storage::{self, Seek};

pub(super) const SCRUB_FRAMES: usize = storage::SCRUB_FRAMES;
const WIDTH: u32 = 320;
const QUALITY: u8 = 5;
/// Keyframe seeks (≈1 s granularity on recorder clips) only once frames are far apart.
const FAST_SEEK_MIN_SECS: f64 = 30.0;

#[cfg(test)]
pub(super) use crate::services::storage::binary;

pub(super) fn frames(path: &str) -> Result<Vec<String>, String> {
    frames_in(&storage::cache_root(), path)
}

/// All eight frames when cached (stats the clip and the cache only).
pub(super) fn cached_in(cache_root: &Path, path: &str) -> Option<Vec<String>> {
    let (id, _, _) = storage::identity_of(Path::new(path)).ok()?;
    let targets = storage::scrub_frame_paths(cache_root, &id);
    targets.iter().all(|t| storage::valid_file(t)).then(|| texts(&targets))
}

fn texts(paths: &[PathBuf]) -> Vec<String> {
    paths.iter().map(|p| p.to_string_lossy().into_owned()).collect()
}

/// Returns cached frames when all eight exist; otherwise extracts the missing ones.
/// Partial success returns the frames that worked; all failing is an error.
pub(super) fn frames_in(cache_root: &Path, path: &str) -> Result<Vec<String>, String> {
    let source = Path::new(path);
    let (id, _, _) = storage::identity_of(source).map_err(|error| format!("scrub source: {error}"))?;
    let targets = storage::scrub_frame_paths(cache_root, &id);
    if targets.iter().all(|t| storage::valid_file(t)) {
        return Ok(texts(&targets));
    }
    std::fs::create_dir_all(storage::visuals_dir(cache_root, &id)).map_err(|error| format!("create scrub cache: {error}"))?;
    let read_from = storage::cached_video_in(cache_root, source).map(PathBuf::from).unwrap_or_else(|| source.to_path_buf());
    let lowered = storage::drive_kind(&read_from).is_hdd();
    let duration = super::mp4_duration(&read_from)
        .or_else(|| storage::probe_duration(&read_from, lowered).ok())
        .filter(|d| d.is_finite() && *d > 0.0)
        .ok_or("clip duration unknown")?;
    let seek = if duration >= FAST_SEEK_MIN_SECS { Seek::Fast } else { Seek::Exact };
    let mut frames = Vec::with_capacity(SCRUB_FRAMES);
    let mut errors = Vec::new();
    for (index, target) in targets.iter().enumerate() {
        let seconds = duration * ((index as f64 + 0.5) / SCRUB_FRAMES as f64);
        let result = if storage::valid_file(target) { Ok(()) } else { storage::extract_frame(&read_from, seconds, WIDTH, QUALITY, target, seek, lowered) };
        match result {
            Ok(()) => frames.push(target.to_string_lossy().into_owned()),
            Err(error) => errors.push(format!("frame {}: {error}", index + 1)),
        }
    }
    if frames.is_empty() { Err(format!("Scrub frames: {}", errors.join("; "))) } else { Ok(frames) }
}
