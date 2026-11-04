//! Storage-aware caching for a clip library that lives on a spinning (often drive-managed
//! SMR) disk while the cache lives on an SSD.
//!
//! * `drive`: classifies each volume as SSD / HDD / SMR HDD (seek-penalty + model query).
//! * `lane`: one serialized I/O lane per physical HDD at background I/O priority, with
//!   on-demand work ahead of idle prefetch. SSD paths skip the lane.
//! * Smart cache (`history`, `score`, `prefetch`, `evict`): interactions are remembered with
//!   decay, every clip gets a score, the idle planner fills the cache to ~95% of ONE budget
//!   (first run: up to 20 GiB, a quarter of free space) in score order and eviction drops the lowest scores first.
//! * SSD tiers under [`cache_root`] (`<exe folder>\cache` on an SSD install):
//!   `thumbs/<id>.jpg` library thumbnails · `visuals/<id>/` poster, filmstrip, scrub frames ·
//!   `editor/<engine key>/` prepared audio stems · `video/<id>.mp4` clip copies (also adopted
//!   from the engine's staging, `kept`) ·
//!   `projects/<id>.json` working edit projects · `tmp/` · `storage.json` settings ·
//!   `media.json` / `interactions.json` bookkeeping.
//!
//! `<id>` is [`identity`] (file name, size, mtime seconds) so a cache entry is found from the
//! directory listing alone, without opening the clip.
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::UNIX_EPOCH;

mod budget;
mod drive;
mod evict;
mod ffmpeg;
pub mod fill;
mod head;
mod history;
mod io_priority;
mod kept;
mod lane;
mod light;
mod prefetch;
mod probe;
mod projects;
pub mod proxy;
mod score;
mod smr;
mod thumbs;
mod tiers;
mod video;
#[cfg(test)]
mod tests;
#[cfg(test)]
mod tests_io;
#[cfg(test)]
mod tests_smart;
#[cfg(test)]
mod tests_smoke;

// Public API for the UI (Settings > Storage, editor playback); not every item is used yet.
#[allow(unused_imports)]
pub use drive::{DriveInfo, DriveKind, drive_info, drive_kind};
pub(crate) use ffmpeg::{Seek, extract_frame, probe_duration};
pub use ffmpeg::{OnLine, Run, binary, run_cancellable, run_cancellable_lines, run_hidden, run_watched};
pub use io_priority::close_helpers;
#[allow(unused_imports)]
pub use head::{ProbeClip, cancel_probe, probe_filter};
#[allow(unused_imports)]
pub use kept::{adopt_kept, sweep_kept};
#[allow(unused_imports)]
pub use light::light_copy;
pub use probe::{ProbedClip, probe_clip};
#[allow(unused_imports)]
pub use evict::DEFAULT_CACHE_BUDGET;
#[allow(unused_imports)]
pub use history::{Interaction, record_interaction};
#[allow(unused_imports)]
pub use lane::{LaneStats, Priority, lane_wait_ms, run_io, stats, submit_io};
#[allow(unused_imports)]
pub use prefetch::{PrefetchClip, PrefetchEvent, PrepareEditor, gpu_work_allowed, recorder_busy, recorder_holds, set_recorder_busy, set_recorder_connected, smart_status, start_prefetch};
pub use projects::{ProjectSource, SidecarStep, export_dir, has_export, project_order, sidecar_step, working_copy_path, write_atomic};
pub(crate) use projects::modified_ms as projects_modified_ms;
#[allow(unused_imports)]
pub use thumbs::{ThumbIndex, cached_thumb, ensure_thumb, recorder_thumb_stem, thumb_index};
#[allow(unused_imports)]
pub use tiers::{CacheKind, CacheUsage, TierUsage, clear_kind, usage_detail};
#[allow(unused_imports)]
pub use video::{cache_video, cached_video, copy_progress, enforce_budget, is_slow, start_fill, too_large};

/// [`cached_video`] against an explicit cache root.
pub fn cached_video_in(root: &Path, source: &Path) -> Option<String> {
    video::cached_in(root, source)
}

/// Settings rows: ("Thumbnails" | "Hover previews" | "Editor media" | "Recent clips", bytes).
/// Never blocks after the first call (a stale snapshot refreshes in the background).
pub fn usage() -> Vec<(&'static str, u64)> {
    let summary = prefetch::summary();
    vec![("Thumbnails", summary.thumbs), ("Hover previews", summary.scrub), ("Editor media", summary.editor), ("Recent clips", summary.video)]
}

/// The one cache budget in bytes (first run: `evict::default_budget`).
pub fn budget_bytes() -> u64 {
    video::budget_in(&cache_root())
}

/// Persists the budget, then evicts down to it and re-plans the fill (in the background).
pub fn set_budget_bytes(bytes: u64) {
    let root = cache_root();
    let mut settings = evict::read_settings(&root);
    settings.cache_budget_bytes = Some(bytes);
    if let Err(error) = evict::write_settings(&root, &settings) {
        eprintln!("save cache budget: {error}");
    }
    let _ = std::thread::Builder::new().name("recall-cache-budget".into()).spawn(|| {
        enforce_budget();
        prefetch::replan();
    });
}

/// Empties every cache tier except what the clip open in the editor uses, and pauses the
/// fill until the next re-plan (list reload or interaction). Returns bytes freed. Reload the
/// clip list afterwards: library tiles pointed at the deleted thumbnails.
pub fn clear() -> Result<u64, String> {
    prefetch::stop();
    let mut freed = 0;
    for kind in CacheKind::ALL {
        freed += clear_kind(kind)?;
    }
    freed += proxy::clear();
    prefetch::invalidate_usage();
    Ok(freed)
}

/// Records which engine folders hold `clip_path`'s editor media; `opened` marks it as the
/// clip in the editor (kept whole by eviction until another clip opens).
pub fn note_editor_media(clip_path: &str, editor_dirs: &[PathBuf], opened: bool) {
    if let Ok((id, _, _)) = identity_of(Path::new(clip_path)) {
        budget::note_editor_media(&cache_root(), &id, editor_dirs, opened);
    }
}

/// Engine media-cache folder for an input (mirrors `recall/src/editor_media.rs::cache_key`,
/// `CACHE_VERSION` 6: FLAC stems): FNV-1a over the canonical path, version, size and mtime ms. Used only
/// to find an existing entry; a mismatch merely costs a re-preparation.
pub fn engine_cache_dir(editor_root: &Path, canonical_input: &Path, bytes: u64, modified_ms: u64) -> PathBuf {
    const CACHE_VERSION: u32 = 6;
    let mut hash = 0xcbf2_9ce4_8422_2325_u64;
    let text = canonical_input.to_string_lossy();
    for byte in text.bytes().chain(CACHE_VERSION.to_le_bytes()).chain(bytes.to_le_bytes()).chain(modified_ms.to_le_bytes()) {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0100_0000_01b3);
    }
    editor_root.join(format!("{hash:016x}"))
}

/// `RECALL_CACHE_DIR`, else `<folder of the running exe>\cache` when that volume is an SSD
/// and writable (the installer puts the app on an SSD), else `%LOCALAPPDATA%\Recall\cache`.
/// Everything the app caches lives under it.
pub fn cache_root() -> PathBuf {
    static ROOT: OnceLock<PathBuf> = OnceLock::new();
    ROOT.get_or_init(|| {
        let root = match std::env::var_os("RECALL_CACHE_DIR").filter(|v| !v.is_empty()) {
            Some(path) => PathBuf::from(path),
            None if cfg!(test) => std::env::temp_dir().join(format!("recall-cache-test-{}", std::process::id())),
            None => {
                let base = std::env::var_os("LOCALAPPDATA").map(PathBuf::from).unwrap_or_else(std::env::temp_dir).join("Recall");
                let root = exe_cache_dir().unwrap_or_else(|| base.join("cache"));
                migrate_editor_cache(&base.join("editor"), &root);
                root
            }
        };
        let _ = std::fs::create_dir_all(&root);
        root
    })
    .clone()
}

/// `<exe folder>\cache` if it is on an SSD and a probe file can be written there.
fn exe_cache_dir() -> Option<PathBuf> {
    let folder = std::env::current_exe().ok()?.parent()?.to_path_buf();
    if drive_kind(&folder) != DriveKind::Ssd {
        return None;
    }
    let root = folder.join("cache");
    std::fs::create_dir_all(&root).ok()?;
    let probe = root.join(format!(".write-probe-{}", std::process::id()));
    std::fs::write(&probe, b"ok").ok()?;
    let _ = std::fs::remove_file(&probe);
    Some(root)
}

/// The pre-tier editor cache (`%LOCALAPPDATA%\Recall\editor`): moved into the root when it
/// is on the same volume (its audio stems stay valid), otherwise deleted — it is regenerable
/// and would otherwise sit outside the budget forever. Its old path-keyed `visuals/` goes.
fn migrate_editor_cache(old: &Path, root: &Path) {
    let target = root.join("editor");
    if !old.is_dir() || target.exists() {
        return;
    }
    let same_volume = drive::volume_root(old) == drive::volume_root(root);
    if same_volume {
        if std::fs::create_dir_all(root).is_ok() && std::fs::rename(old, &target).is_ok() {
            let _ = std::fs::remove_dir_all(target.join("visuals"));
        }
    } else {
        let _ = std::fs::remove_dir_all(old);
    }
}

/// Cache key for one clip revision: FNV-1a over the lowercase file name, byte length and
/// modification time (whole seconds). Everything comes from a directory listing, so a
/// lookup never touches the clip itself.
pub fn identity(file_name: &str, bytes: u64, mtime_secs: u64) -> String {
    let mut hash = 0xcbf2_9ce4_8422_2325_u64;
    let mut feed = |data: &[u8]| {
        for byte in data {
            hash ^= u64::from(*byte);
            hash = hash.wrapping_mul(0x0100_0000_01b3);
        }
    };
    feed(file_name.to_lowercase().as_bytes());
    feed(&[0]);
    feed(&bytes.to_le_bytes());
    feed(&mtime_secs.to_le_bytes());
    format!("{hash:016x}")
}

/// Stats `path` (metadata only) and returns its identity plus (bytes, mtime seconds).
pub fn identity_of(path: &Path) -> Result<(String, u64, u64), String> {
    let metadata = path.metadata().map_err(|error| format!("stat {}: {error}", path.display()))?;
    if !metadata.is_file() {
        return Err(format!("{} is not a file", path.display()));
    }
    let name = file_name(path).ok_or("clip has no file name")?;
    let secs = mtime_secs(&metadata);
    Ok((identity(&name, metadata.len(), secs), metadata.len(), secs))
}

pub(crate) fn mtime_secs(metadata: &std::fs::Metadata) -> u64 {
    metadata.modified().unwrap_or(UNIX_EPOCH).duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

pub(crate) fn file_name(path: &Path) -> Option<String> {
    path.file_name().map(|name| name.to_string_lossy().into_owned())
}

/// Hover-scrub frames per clip.
pub const SCRUB_FRAMES: usize = 8;

/// `visuals/<id>/`: editor poster + filmstrip and hover-scrub frames of one clip revision.
pub fn visuals_dir(root: &Path, id: &str) -> PathBuf {
    root.join("visuals").join(id)
}

/// `visuals/<id>/scrub-<i>.jpg` for i in 0..8.
pub fn scrub_frame_paths(root: &Path, id: &str) -> Vec<PathBuf> {
    let directory = visuals_dir(root, id);
    (0..SCRUB_FRAMES).map(|index| directory.join(format!("scrub-{index}.jpg"))).collect()
}

/// A cache file counts only when it is a non-trivial regular file (a crashed writer can
/// leave an empty one behind).
pub(crate) fn valid_file(path: &Path) -> bool {
    path.metadata().map(|m| m.is_file() && m.len() > 100).unwrap_or(false)
}

/// `\\?\V:\x` → `V:\x` for display and volume lookups.
pub fn plain_path(path: &Path) -> PathBuf {
    let text = path.to_string_lossy();
    match text.strip_prefix(r"\\?\") {
        Some(rest) if rest.starts_with("UNC\\") => PathBuf::from(format!(r"\\{}", &rest[4..])),
        Some(rest) => PathBuf::from(rest),
        None => path.to_path_buf(),
    }
}

/// Unique temp sibling name for atomic publish (`<name>.<pid>-<nanos>.part`).
pub(crate) fn temp_sibling(target: &Path) -> PathBuf {
    let nonce = std::time::SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_nanos();
    let name = target.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "cache".into());
    target.with_file_name(format!("{name}.{}-{nonce}.part", std::process::id()))
}
