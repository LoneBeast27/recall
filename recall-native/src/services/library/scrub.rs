//! Hover-scrub frames: 8 JPEGs at (i+0.5)/8 of the clip, cached on the SSD at
//! `<cache>/visuals/<identity>/scrub-<i>.jpg` (the folder the editor's visuals share).
//! They replace the thumbnail in the same tile while hovered, so they are made as wide as
//! the thumbnail ([`storage::thumb_width`]: the widest tile the chosen grid size can show on
//! any monitor, in physical pixels). Frames narrower than that (made for smaller tiles, or by
//! a version that always made 320 px ones) are made again.
//!
//! Frames come from the SSD video copy when one exists; otherwise from the clip itself with
//! fast (keyframe) seeks and ffmpeg at very low I/O priority. Callers route HDD clips
//! through the disk lane (`worker.rs`).
use std::path::{Path, PathBuf};

use crate::services::storage::{self, Seek};

pub(super) const SCRUB_FRAMES: usize = storage::SCRUB_FRAMES;
const QUALITY: u8 = 5;
/// Keyframe seeks (≈1 s granularity on recorder clips) only once frames are far apart.
const FAST_SEEK_MIN_SECS: f64 = 30.0;

#[cfg(test)]
pub(super) use crate::services::storage::binary;

pub(super) fn frames(path: &str) -> Result<Vec<String>, String> {
    frames_in(&storage::cache_root(), path, storage::thumb_width())
}

/// A frame file that is there and at least `width` px wide.
fn sharp(target: &Path, width: u32) -> bool {
    storage::valid_file(target) && storage::thumb_file_width(target).is_some_and(|w| w >= width)
}

/// All eight frames when cached and the first is at least `width` px wide (the eight are
/// always made together at one width). Stats the clip and the cache, reads one JPEG header.
pub(super) fn cached_in(cache_root: &Path, path: &str, width: u32) -> Option<Vec<String>> {
    let (id, _, _) = storage::identity_of(Path::new(path)).ok()?;
    let targets = storage::scrub_frame_paths(cache_root, &id);
    let all = targets.iter().all(|t| storage::valid_file(t)) && sharp(&targets[0], width);
    all.then(|| texts(&targets))
}

fn texts(paths: &[PathBuf]) -> Vec<String> {
    paths.iter().map(|p| p.to_string_lossy().into_owned()).collect()
}

/// Returns cached frames when all eight exist; otherwise extracts the missing ones.
/// Partial success returns the frames that worked; all failing is an error.
pub(super) fn frames_in(cache_root: &Path, path: &str, width: u32) -> Result<Vec<String>, String> {
    let source = Path::new(path);
    let (id, _, _) = storage::identity_of(source).map_err(|error| format!("scrub source: {error}"))?;
    let targets = storage::scrub_frame_paths(cache_root, &id);
    if targets.iter().all(|t| sharp(t, width)) {
        return Ok(texts(&targets));
    }
    // Narrower frames go first: an existing valid file wins the publish race.
    for target in targets.iter().filter(|t| storage::valid_file(t) && !sharp(t, width)) {
        let _ = std::fs::remove_file(target);
    }
    std::fs::create_dir_all(storage::visuals_dir(cache_root, &id)).map_err(|error| format!("create scrub cache: {error}"))?;
    let read_from = storage::cached_video_in(cache_root, source).map(PathBuf::from).unwrap_or_else(|| source.to_path_buf());
    let lowered = storage::drive_kind(&read_from).is_hdd();
    let duration = super::mp4_duration(&read_from)
        .or_else(|| storage::probe_duration(&read_from, lowered).ok())
        .filter(|d| d.is_finite() && *d > 0.0)
        .ok_or("clip duration unknown")?;
    let seek = if duration >= FAST_SEEK_MIN_SECS { Seek::Fast } else { Seek::Exact };
    let at = |index: usize| duration * ((index as f64 + 0.5) / SCRUB_FRAMES as f64);
    // Off a hard drive the missing frames come from one ffmpeg process; any it did not
    // write (and every frame of a hard-drive clip) is extracted one by one below.
    if !lowered {
        let missing: Vec<(f64, PathBuf)> = targets.iter().enumerate().filter(|(_, t)| !storage::valid_file(t)).map(|(index, t)| (at(index), t.clone())).collect();
        if missing.len() > 1 {
            storage::extract_frames(&read_from, &missing, width, QUALITY, seek);
        }
    }
    let mut frames = Vec::with_capacity(SCRUB_FRAMES);
    let mut errors = Vec::new();
    for (index, target) in targets.iter().enumerate() {
        let seconds = at(index);
        let result = if storage::valid_file(target) { Ok(()) } else { storage::extract_frame(&read_from, seconds, width, QUALITY, target, seek, lowered) };
        match result {
            Ok(()) => frames.push(target.to_string_lossy().into_owned()),
            Err(error) => errors.push(format!("frame {}: {error}", index + 1)),
        }
    }
    if frames.is_empty() { Err(format!("Scrub frames: {}", errors.join("; "))) } else { Ok(frames) }
}
