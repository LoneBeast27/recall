//! Low free-space warning for the clip drive and the engine's SSD staging
//! drive (works with or without a storage cap). With each storage run (after
//! a save, hourly, on demand) and once shortly after start, the
//! `recall-storage` thread reads the real free space of both volumes. Below
//! [`threshold`] = max(2 x the replay buffer's bytes, 5 GiB) it sends
//! `{"type":"storage_warning","level":"free","drive":"C:","free_bytes":F,
//! "need_bytes":T}` on `mod:"session"`, once per crossing; a drive warns again
//! only after it climbed [`REARM_BYTES`] above the threshold.

use std::collections::HashSet;
use std::path::{Component, Path, PathBuf, Prefix};

use serde_json::json;

use crate::record_storage::GIB;

/// The lowest warning threshold, whatever the buffer.
pub const FREE_FLOOR: u64 = 5 * GIB;
/// A warned drive re-arms once its free space is this far above the threshold.
pub const REARM_BYTES: u64 = GIB;

/// `fn(folder) -> free bytes on its volume` (`None` = unknown, no warning).
pub type FreeFn = fn(&Path) -> Option<u64>;

/// Bytes the replay buffer holds at the configured video bitrate.
pub fn buffer_bytes(buffer_secs: u32, bitrate_mbps: u32) -> u64 {
    u64::from(buffer_secs) * u64::from(bitrate_mbps) * 1_000_000 / 8
}

/// Warn below this much free space: max(2 x buffer, [`FREE_FLOOR`]).
pub fn threshold(buffer_bytes: u64) -> u64 {
    buffer_bytes.saturating_mul(2).max(FREE_FLOOR)
}

/// `C:` for drive paths, `\\server\share` for UNC (as the engine names them).
pub fn drive_label(path: &Path) -> String {
    let abs = std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf());
    match abs.components().next() {
        Some(Component::Prefix(p)) => match p.kind() {
            Prefix::Disk(d) | Prefix::VerbatimDisk(d) => format!("{}:", d.to_ascii_uppercase() as char),
            Prefix::UNC(server, share) | Prefix::VerbatimUNC(server, share) => {
                format!(r"\\{}\{}", server.to_string_lossy(), share.to_string_lossy()).to_lowercase()
            }
            _ => p.as_os_str().to_string_lossy().to_lowercase(),
        },
        _ => path.display().to_string(),
    }
}

/// The engine's staging folder (mirrors `recall/src/encoder/staging.rs`):
/// `RECALL_STAGING_DIR`, `off` = none, else `<engine dir>\cache\staging`.
pub fn staging_dir(env: Option<&str>, engine: Option<&Path>) -> Option<PathBuf> {
    match env.map(str::trim) {
        Some(v) if v.eq_ignore_ascii_case("off") => None,
        Some(v) if !v.is_empty() => Some(PathBuf::from(v)),
        _ => engine.and_then(Path::parent).map(|d| d.join("cache").join("staging")),
    }
}

/// [`staging_dir`] for the engine this controller runs.
pub fn engine_staging_dir() -> Option<PathBuf> {
    let engine = std::env::current_exe().ok().and_then(|exe| crate::supervisor::locate_sibling_from(&exe, "recall.exe"));
    staging_dir(std::env::var("RECALL_STAGING_DIR").ok().as_deref(), engine.as_deref())
}

/// The drives one check reads (one entry per volume) and its threshold.
#[derive(Debug, Clone, PartialEq)]
pub struct FreeCheck {
    pub drives: Vec<(String, PathBuf)>,
    pub threshold: u64,
}

impl FreeCheck {
    pub fn new(clip_dir: &Path, staging: Option<&Path>, buffer_bytes: u64) -> Self {
        let mut drives: Vec<(String, PathBuf)> = Vec::new();
        for dir in std::iter::once(clip_dir).chain(staging) {
            let label = drive_label(dir);
            if !drives.iter().any(|(l, _)| *l == label) {
                drives.push((label, dir.to_path_buf()));
            }
        }
        FreeCheck { drives, threshold: threshold(buffer_bytes) }
    }
}

/// One warning per drive per crossing.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct FreeWarner {
    low: HashSet<String>,
}

impl FreeWarner {
    /// The (drive, free bytes) to warn about now.
    pub fn update(&mut self, check: &FreeCheck, free: FreeFn) -> Vec<(String, u64)> {
        let mut out = Vec::new();
        for (label, dir) in &check.drives {
            let Some(have) = free(dir) else { continue };
            if have < check.threshold {
                if self.low.insert(label.clone()) {
                    out.push((label.clone(), have));
                }
            } else if have >= check.threshold.saturating_add(REARM_BYTES) {
                self.low.remove(label);
            }
        }
        out
    }
}

pub fn free_event(drive: &str, free_bytes: u64, need_bytes: u64) -> String {
    json!({"type": "storage_warning", "level": "free", "drive": drive, "free_bytes": free_bytes, "need_bytes": need_bytes})
        .to_string()
}

/// Free bytes for the caller on the volume holding `dir` (its nearest
/// existing ancestor is asked, so a folder not made yet still answers).
pub fn free_bytes(dir: &Path) -> Option<u64> {
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
    .map(fake_free)
}

/// Checks only: `RECALL_FAKE_FREE_BYTES=<n>` caps every drive's free space at n
/// (the low-disk fixture; the app's drive info reads the same variable).
fn fake_free(free: u64) -> u64 {
    std::env::var("RECALL_FAKE_FREE_BYTES").ok().and_then(|v| v.trim().parse::<u64>().ok()).map_or(free, |fake| fake.min(free))
}

#[cfg(test)]
#[path = "record_free_space_tests.rs"]
mod tests;
