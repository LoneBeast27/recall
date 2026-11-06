//! Free space for a save. Before muxing, the saver checks the drive it is
//! about to write (estimate: the window's video bytes + AAC audio + the MP4
//! index + a small fixed allowance, see [`estimate`]).
//! A staged save whose staging drive is short, or whose staged mux fails with
//! a disk-full error (ERROR_DISK_FULL 112 / ERROR_HANDLE_DISK_FULL 39), is
//! written in place on the clip drive instead. When that drive is short too
//! the save fails with "Not enough space on <staging> or <clip> (need X)".
//!
//! The free-space function is injected ([`FreeSpace`]) so tests never fill a
//! disk; production uses [`free_bytes`] (`win_volume.rs`). An unknown
//! answer counts as enough room, so the worst case is today's behaviour.

use std::path::{Path, PathBuf};

use super::saver::{SaveJob, VideoSrc};
use super::staging::volume_key;

/// `fn(folder) -> free bytes` on that folder's volume (`None` = unknown).
pub(crate) type FreeSpace = fn(&Path) -> Option<u64>;

/// Free space of a save folder; the folder is resolved once per minute per
/// thread (`win_volume::VolumeCache`), its free space read every save.
pub(crate) use crate::win_volume::free_bytes_cached as free_bytes;

/// MP4 index bytes per sample, worst case: stsz 4, stts 8, ctts 8, stss 4 and
/// one co64 chunk offset (8) per sample. FFmpeg's mov muxer writes no
/// per-packet bytes into `mdat`, so this is all the container adds per frame.
const INDEX_BYTES_PER_SAMPLE: u64 = 32;
/// ftyp + moov box headers and track metadata (a few KB) and NTFS cluster
/// rounding of the clip file. Only the clip itself is reserved: the gallery
/// thumbnail and bookmark sidecar are not, since a failed thumbnail does not
/// fail the save.
const HEADER_BYTES: u64 = 64 << 10;
/// AAC bytes per second per track: `mux.rs` asks for 192 kbit/s and the
/// encoder averages to it; 10% over is allowed (an allowance, not measured).
const AAC_BYTES_PER_SEC: f64 = 192_000.0 / 8.0 * 1.1;
const AAC_SAMPLES_PER_FRAME: f64 = 1024.0;

/// Bytes this save is expected to need on disk: the video as it is in the
/// window (stream copy) or the still at its bitrate, the AAC audio, the MP4
/// index for every video and audio sample, and [`HEADER_BYTES`].
pub(crate) fn estimate(job: &SaveJob) -> u64 {
    let secs = job.secs.max(0.0);
    let (video, video_samples) = match &job.video {
        VideoSrc::Window { packets, .. } => (packets.iter().map(|p| p.data.len() as u64).sum(), packets.len() as u64),
        VideoSrc::Still { spec, .. } => ((spec.bitrate as f64 / 8.0 * secs) as u64, (f64::from(spec.fps) * secs).ceil() as u64),
    };
    let audio = (job.audio.len() as f64 * AAC_BYTES_PER_SEC * secs) as u64;
    let audio_frames: u64 = job.audio.iter().map(|t| (f64::from(t.rate) / AAC_SAMPLES_PER_FRAME * secs).ceil() as u64).sum();
    video + audio + (video_samples + audio_frames) * INDEX_BYTES_PER_SAMPLE + HEADER_BYTES
}

/// `C:` / `\\server\share` for a path, as the user knows the drive.
/// The volume the file's folder really lands on (`win_volume.rs`, cached per
/// folder on this thread), else the path text.
pub(crate) fn drive(path: &Path) -> String {
    crate::win_volume::volume_of_cached(folder(path))
        .map(|v| v.label)
        .or_else(|| volume_key(path))
        .unwrap_or_else(|| path.display().to_string())
}

/// The folder a file is written into (`.` for a bare name).
fn folder(file: &Path) -> &Path {
    file.parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or(Path::new("."))
}

fn room(free: FreeSpace, file: &Path, need: u64) -> bool {
    !matches!(free(folder(file)), Some(have) if have < need)
}

/// "1.5 GB" / "300 MB": decimal units like the app's sizes, rounded up so
/// the message never asks for less than is needed.
pub(crate) fn size(bytes: u64) -> String {
    if bytes >= 1_000_000_000 {
        format!("{:.1} GB", bytes.div_ceil(100_000_000) as f64 / 10.0)
    } else {
        format!("{} MB", bytes.div_ceil(1_000_000))
    }
}

/// "Not enough space on F: or E: (need 1.4 GB)"; one drive named once.
pub(crate) fn no_room(files: &[&Path], need: u64) -> String {
    let mut drives: Vec<String> = Vec::new();
    for d in files.iter().map(|f| drive(f)) {
        if !drives.contains(&d) {
            drives.push(d);
        }
    }
    format!("Not enough space on {} (need {})", drives.join(" or "), size(need))
}

/// A mux / rename error that means the volume is full.
pub(crate) fn is_disk_full(reason: &str) -> bool {
    let r = reason.to_ascii_lowercase();
    ["os error 112", "os error 39", "no space left on device", "not enough space on the disk", "disk is full"]
        .iter()
        .any(|s| r.contains(s))
}

/// Where a save goes once free space is known: `Ok(Some(staged))`,
/// `Ok(None)` = in place, or the "Not enough space" reason. The second value
/// is a log line when the staging drive was skipped.
pub(crate) fn choose(
    staged: Option<PathBuf>,
    final_path: &Path,
    need: u64,
    free: FreeSpace,
) -> Result<(Option<PathBuf>, Option<String>), String> {
    let Some(staged) = staged else {
        return if room(free, final_path, need) { Ok((None, None)) } else { Err(no_room(&[final_path], need)) };
    };
    if room(free, &staged, need) {
        return Ok((Some(staged), None));
    }
    if !room(free, final_path, need) {
        return Err(no_room(&[&staged, final_path], need));
    }
    let note = format!("staging drive {} is short of space (need {}); writing the clip in place", drive(&staged), size(need));
    Ok((None, Some(note)))
}

/// Write the save to its chosen target with `write(target)`; a staged write
/// that fails disk-full is retried once in place when the clip drive has
/// room. Returns the target that was written (or tried last) and the result.
pub(crate) fn write_with_room<T>(
    staged: Option<PathBuf>,
    final_path: &Path,
    need: u64,
    free: FreeSpace,
    log: &dyn Fn(&str),
    mut write: impl FnMut(&Path) -> Result<T, String>,
) -> (PathBuf, Result<T, String>) {
    let staged = match choose(staged, final_path, need, free) {
        Ok((staged, note)) => {
            if let Some(note) = note {
                log(&note);
            }
            staged
        }
        Err(reason) => return (final_path.to_path_buf(), Err(reason)),
    };
    let Some(staged) = staged else { return (final_path.to_path_buf(), write(final_path)) };
    match write(&staged) {
        Err(reason) if is_disk_full(&reason) => {
            if !room(free, final_path, need) {
                return (staged.clone(), Err(no_room(&[&staged, final_path], need)));
            }
            log(&format!("staging drive {} filled up while writing ({reason}); writing the clip in place", drive(&staged)));
            (final_path.to_path_buf(), write(final_path))
        }
        other => (staged, other),
    }
}

#[cfg(test)]
#[path = "space_tests.rs"]
mod tests;
