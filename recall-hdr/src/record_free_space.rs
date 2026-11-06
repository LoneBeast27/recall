//! Low free-space warning for the clip drive and the engine's SSD staging
//! drive (works with or without a storage cap). With each storage run (after
//! a save, hourly, on demand) and once shortly after start, the
//! `recall-storage` thread reads the real free space of both volumes. Below
//! [`threshold`] = max(2 x the replay buffer's bytes, 5 GB) it sends
//! `{"type":"storage_warning","level":"free","drive":"C:","free_bytes":F,
//! "need_bytes":T}` on `mod:"session"`, once per crossing; a drive warns again
//! only after it climbed [`REARM_BYTES`] above the threshold. Drives are the
//! volumes the folders really land on (`win_volume.rs`: junctions, mounted
//! folders and subst drives resolve), so a junctioned staging folder on the
//! clip volume is read once. Resolving a folder can block (a sleeping NAS,
//! a spun-down disk), so it happens only on the storage thread
//! ([`resolve_drives`]); a [`FreeCheck`] built on the pump holds plain paths.

use std::collections::HashSet;
use std::path::{Component, Path, PathBuf, Prefix};

use serde_json::json;


/// The lowest warning threshold, whatever the buffer: 5 GB decimal, so the
/// app (decimal sizes) shows "5.00 GB", not "5.37 GB".
pub const FREE_FLOOR: u64 = 5_000_000_000;
/// A warned drive re-arms once its free space is this far above the threshold.
pub const REARM_BYTES: u64 = 1_000_000_000;

/// `fn(folder) -> free bytes on its volume` (`None` = unknown, no warning).
pub type FreeFn = fn(&Path) -> Option<u64>;
/// `fn(folder) -> the volume it really lands on` (may block; storage thread only).
pub type Resolve = fn(&Path) -> Option<crate::win_volume::Volume>;

/// Bytes the replay buffer holds at the configured video bitrate.
pub fn buffer_bytes(buffer_secs: u32, bitrate_mbps: u32) -> u64 {
    u64::from(buffer_secs) * u64::from(bitrate_mbps) * 1_000_000 / 8
}

/// Warn below this much free space: max(2 x buffer, [`FREE_FLOOR`]).
pub fn threshold(buffer_bytes: u64) -> u64 {
    buffer_bytes.saturating_mul(2).max(FREE_FLOOR)
}

/// The drive as the path text names it (a folder Windows cannot resolve).
fn text_label(path: &Path) -> String {
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

/// The folders one check reads (clip folder, then staging) and its threshold.
/// Plain data: building one touches no filesystem (the pump builds it).
#[derive(Debug, Clone, PartialEq)]
pub struct FreeCheck {
    pub dirs: Vec<PathBuf>,
    pub threshold: u64,
}

impl FreeCheck {
    pub fn new(clip_dir: &Path, staging: Option<&Path>, buffer_bytes: u64) -> Self {
        let dirs = std::iter::once(clip_dir).chain(staging).map(Path::to_path_buf).collect();
        FreeCheck { dirs, threshold: threshold(buffer_bytes) }
    }
}

/// (label, folder) per volume the folders land on, first folder per volume
/// kept. Storage thread only: `resolve` may block.
pub fn resolve_drives(dirs: &[PathBuf], resolve: Resolve) -> Vec<(String, PathBuf)> {
    let mut drives: Vec<(String, PathBuf)> = Vec::new();
    let mut seen: Vec<String> = Vec::new();
    for dir in dirs {
        let (key, label) = match resolve(dir) {
            Some(v) => (v.id, v.label),
            None => (text_label(dir), text_label(dir)),
        };
        if !seen.contains(&key) {
            seen.push(key);
            drives.push((label, dir.clone()));
        }
    }
    drives
}

/// One warning per drive per crossing.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct FreeWarner {
    low: HashSet<String>,
}

impl FreeWarner {
    /// The (drive, free bytes) to warn about now.
    pub fn update(&mut self, drives: &[(String, PathBuf)], threshold: u64, free: FreeFn) -> Vec<(String, u64)> {
        let mut out = Vec::new();
        for (label, dir) in drives {
            let Some(have) = free(dir) else { continue };
            if have < threshold {
                if self.low.insert(label.clone()) {
                    out.push((label.clone(), have));
                }
            } else if have >= threshold.saturating_add(REARM_BYTES) {
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

/// Free bytes on the volume `dir` really lands on (`RECALL_FAKE_FREE_BYTES` honoured).
pub use crate::win_volume::free_bytes;

#[cfg(test)]
#[path = "record_free_space_tests.rs"]
mod tests;
