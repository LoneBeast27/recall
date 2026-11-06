//! Free space for a save. Before muxing, the saver checks the drive it is
//! about to write (estimate: the window's video bytes + AAC audio + 64 MiB).
//! A staged save whose staging drive is short, or whose staged mux fails with
//! a disk-full error (ERROR_DISK_FULL 112 / ERROR_HANDLE_DISK_FULL 39), is
//! written in place on the clip drive instead. When that drive is short too
//! the save fails with "Not enough space on <staging> or <clip> (need X)".
//!
//! The free-space function is injected ([`FreeSpace`]) so tests never fill a
//! disk; production uses [`free_bytes`] (`GetDiskFreeSpaceExW`). An unknown
//! answer counts as enough room, so the worst case is today's behaviour.

use std::path::{Path, PathBuf};

use super::saver::{SaveJob, VideoSrc};
use super::staging::volume_key;

/// `fn(folder) -> free bytes` on that folder's volume (`None` = unknown).
pub(crate) type FreeSpace = fn(&Path) -> Option<u64>;

/// Headroom on top of the clip estimate (moov atom, thumbnails, rounding).
pub(crate) const MARGIN: u64 = 64 << 20;
/// AAC bytes per second per track (`mux.rs` encodes at 192 kbit/s).
const AAC_BYTES_PER_SEC: f64 = 192_000.0 / 8.0;

/// Bytes this save is expected to need on disk.
pub(crate) fn estimate(job: &SaveJob) -> u64 {
    let video = match &job.video {
        VideoSrc::Window { packets, .. } => packets.iter().map(|p| p.data.len() as u64).sum(),
        VideoSrc::Still { spec, .. } => (spec.bitrate as f64 / 8.0 * job.secs.max(0.0)) as u64,
    };
    let audio = (job.audio.len() as f64 * AAC_BYTES_PER_SEC * job.secs.max(0.0)) as u64;
    video + audio + MARGIN
}

/// `C:` / `\\server\share` for a path, as the user knows the drive.
pub(crate) fn drive(path: &Path) -> String {
    volume_key(path).unwrap_or_else(|| path.display().to_string())
}

/// The folder a file is written into (`.` for a bare name).
fn folder(file: &Path) -> &Path {
    file.parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or(Path::new("."))
}

fn room(free: FreeSpace, file: &Path, need: u64) -> bool {
    !matches!(free(folder(file)), Some(have) if have < need)
}

/// "1.4 GB" / "64 MB".
pub(crate) fn size(bytes: u64) -> String {
    const GIB: u64 = 1 << 30;
    if bytes >= GIB {
        format!("{:.1} GB", bytes as f64 / GIB as f64)
    } else {
        format!("{} MB", bytes.div_ceil(1 << 20))
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

/// Free bytes for the caller on the volume holding `dir` (the nearest
/// existing ancestor is asked, so a folder not made yet still answers).
pub(crate) fn free_bytes(dir: &Path) -> Option<u64> {
    use std::os::windows::ffi::OsStrExt;
    #[link(name = "kernel32", kind = "raw-dylib")]
    extern "system" {
        fn GetDiskFreeSpaceExW(dir: *const u16, free_to_caller: *mut u64, total: *mut u64, total_free: *mut u64) -> i32;
    }
    let abs = std::path::absolute(dir).ok()?;
    abs.ancestors().filter(|p| p.is_dir()).find_map(|p| {
        let wide: Vec<u16> = p.as_os_str().encode_wide().chain(std::iter::once(0)).collect();
        let mut free = 0u64;
        // SAFETY: NUL-terminated path; the out pointer is valid, the others null.
        let ok = unsafe { GetDiskFreeSpaceExW(wide.as_ptr(), &mut free, std::ptr::null_mut(), std::ptr::null_mut()) };
        (ok != 0).then_some(free)
    })
}

#[cfg(test)]
#[path = "space_tests.rs"]
mod tests;
