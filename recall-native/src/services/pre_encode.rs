//! Pre-encode: the re-encode export of the open clip, run in the background into the cache
//! once its edits or export settings settle, so Export only has to copy the finished file into
//! `Recall\Edited`.
//!
//! One prepared file at a time, keyed by everything that shapes the output (clip identity,
//! plan, fps, export settings, engine build). It runs through the same engine path as Export
//! (below-normal priority, the engine's background-CPU pin, very low I/O priority on an HDD)
//! and never alongside a real export. Its folder (`cache/tmp/pre-encode`) is emptied at app
//! start, editor close and app close.
use std::{
    hash::{Hash, Hasher},
    path::{Path, PathBuf},
    sync::{
        Condvar, Mutex, MutexGuard,
        atomic::{AtomicBool, AtomicU32, Ordering},
    },
    time::Duration,
};

use crate::{
    backend::{self, ExportSummary},
    edit_model::{ClipInfo, EditPlan, ExportMode, ExportSettings},
    services::storage::{self, Priority},
};

/// A finished pre-encode in the cache.
#[derive(Debug, Clone)]
pub struct Prepared {
    pub key: u64,
    pub path: PathBuf,
    pub summary: ExportSummary,
}

/// What the panel shows for one key.
#[derive(Debug, Clone, PartialEq)]
pub enum Status {
    /// Nothing prepared or running for this key.
    None,
    /// Running; the share done when the engine reports it.
    Running(Option<f32>),
    Ready,
    Failed(String),
}

#[derive(Default)]
struct Slot {
    running: Option<u64>,
    ready: Option<Prepared>,
    failed: Option<(u64, String)>,
}

static SLOT: Mutex<Slot> = Mutex::new(Slot { running: None, ready: None, failed: None });
static ENDED: Condvar = Condvar::new();
static CANCEL: AtomicBool = AtomicBool::new(false);
/// Per-mille done of the running pre-encode; `UNKNOWN` until the engine reports.
static PROGRESS: AtomicU32 = AtomicU32::new(UNKNOWN);
const UNKNOWN: u32 = u32::MAX;

fn slot() -> MutexGuard<'static, Slot> {
    SLOT.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

pub fn dir() -> PathBuf {
    storage::cache_root().join("tmp").join("pre-encode")
}

/// Everything that shapes the output. `None` for a lossless export (already a fast copy).
pub fn key(info: &ClipInfo, plan: &EditPlan, fps: u32, export: &ExportSettings) -> Option<u64> {
    if export.mode != ExportMode::Reencode {
        return None;
    }
    let engine = backend::engine_path().ok().map(|path| {
        let modified = std::fs::metadata(&path).and_then(|m| m.modified()).ok();
        (path, modified)
    });
    let shape = serde_json::to_string(&(plan, export)).ok()?;
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    (&info.path, info.source_bytes, info.source_modified_ms, shape, fps, engine).hash(&mut hasher);
    Some(hasher.finish())
}

pub fn status(key: u64) -> Status {
    let slot = slot();
    if slot.running == Some(key) {
        let progress = PROGRESS.load(Ordering::Relaxed);
        return Status::Running((progress != UNKNOWN).then(|| progress as f32 / 1000.0));
    }
    if slot.ready.as_ref().is_some_and(|ready| ready.key == key) {
        return Status::Ready;
    }
    match &slot.failed {
        Some((failed, error)) if *failed == key => Status::Failed(error.clone()),
        _ => Status::None,
    }
}

pub fn running() -> bool {
    slot().running.is_some()
}

/// Starts preparing `key` on a thread of its own. Drops any other prepared file first (one at
/// a time). Refused while another pre-encode still runs: cancel it and try again next tick.
pub fn start(key: u64, info: ClipInfo, plan: EditPlan, fps: u32, export: ExportSettings) -> bool {
    {
        let mut slot = slot();
        if slot.running.is_some() {
            return false;
        }
        slot.ready = None;
        slot.failed = None;
        slot.running = Some(key);
    }
    clear_dir();
    CANCEL.store(false, Ordering::Release);
    PROGRESS.store(UNKNOWN, Ordering::Relaxed);
    let body = move || {
        let result = prepare(key, &info, &plan, fps, &export);
        let mut slot = slot();
        slot.running = None;
        match result {
            Ok(prepared) if !CANCEL.load(Ordering::Acquire) => slot.ready = Some(prepared),
            Ok(prepared) => {
                let _ = std::fs::remove_file(prepared.path);
            }
            Err(error) if error == backend::EXPORT_CANCELLED => {}
            Err(error) => slot.failed = Some((key, error)),
        }
        ENDED.notify_all();
    };
    if let Err(error) = std::thread::Builder::new().name("recall-pre-encode".into()).spawn(body) {
        let mut slot = slot();
        slot.running = None;
        slot.failed = Some((key, format!("start pre-encode: {error}")));
        return false;
    }
    true
}

fn prepare(key: u64, info: &ClipInfo, plan: &EditPlan, fps: u32, export: &ExportSettings) -> Result<Prepared, String> {
    let folder = dir();
    std::fs::create_dir_all(&folder).map_err(|e| format!("create pre-encode folder: {e}"))?;
    let path = folder.join(format!("{key:016x}.mp4"));
    storage::mark_owned(&storage::cache_root(), &format!("tmp/pre-encode/{key:016x}.mp4"));
    let on_line: storage::OnLine = Box::new(|line| {
        if let Some(share) = progress_share(line) {
            PROGRESS.store((share * 1000.0) as u32, Ordering::Relaxed);
        }
    });
    let summary = backend::engine_export(info, &path, plan, fps, export, &CANCEL, Some(on_line), true)?;
    Ok(Prepared { key, path, summary })
}

/// The share done from an engine `export_progress` line; other lines give `None`.
pub fn progress_share(line: &str) -> Option<f32> {
    let event: serde_json::Value = serde_json::from_str(line).ok()?;
    if event["type"] != "export_progress" {
        return None;
    }
    let (frames, total) = (event["frames"].as_u64()?, event["total"].as_u64()?);
    (total > 0).then(|| (frames as f32 / total as f32).clamp(0.0, 1.0))
}

/// Stops the running pre-encode unless it is for `keep`, and drops a prepared file that is not.
pub fn cancel_other(keep: Option<u64>) {
    let mut slot = slot();
    if slot.running.is_some() && slot.running != keep {
        CANCEL.store(true, Ordering::Release);
    }
    if slot.ready.as_ref().is_some_and(|ready| Some(ready.key) != keep) {
        if let Some(ready) = slot.ready.take() {
            let _ = std::fs::remove_file(ready.path);
        }
    }
    if slot.failed.as_ref().is_some_and(|(failed, _)| Some(*failed) != keep) {
        slot.failed = None;
    }
}

/// Editor or app closing: stop, wait (bounded) and empty the folder.
pub fn discard() {
    cancel_other(None);
    drop(wait_idle(slot(), Duration::from_secs(10)));
    clear_dir();
}

/// App start: anything left in the folder is from an earlier run.
/// Suffix of a prepared file still being copied into the export folder (`.<name><suffix>`).
const DELIVERING: &str = ".recall-pre-encode";

/// App start: a copy into the export folder cut off by a crash leaves a hidden
/// `.<name>.recall-pre-encode` there. Removes exactly those, off the UI thread and on the
/// folder's drive lane.
pub fn sweep_deliveries() {
    let body = || {
        let recordings = backend::library_dir();
        let Some(folder) = storage::export_dir(&recordings.join("clip.mp4"), &recordings) else { return };
        let lane = folder.clone();
        storage::submit_io(&lane, Priority::Background, move || {
            remove_deliveries(&folder);
        });
    };
    let _ = std::thread::Builder::new().name("recall-delivery-sweep".into()).spawn(body);
}

/// Deletes the cut-off copies in `folder`; nothing else. Returns how many went.
fn remove_deliveries(folder: &Path) -> usize {
    let Ok(entries) = std::fs::read_dir(folder) else { return 0 };
    entries
        .flatten()
        .filter(|entry| {
            let name = entry.file_name().to_string_lossy().into_owned();
            name.starts_with('.') && name.ends_with(DELIVERING) && name.len() > DELIVERING.len() + 1
        })
        .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_file()))
        .filter(|entry| std::fs::remove_file(entry.path()).is_ok())
        .count()
}

pub fn clear_dir() {
    let ready = slot().ready.as_ref().map(|ready| ready.path.clone());
    clear_in(&storage::cache_root(), ready.as_deref());
}

/// [`clear_dir`] of the cache at `root`, sparing `ready`: only prepared files Recall listed
/// (`<key>.mp4`) and the engine's temporaries of them; anything else in the folder stays.
pub(crate) fn clear_in(root: &Path, ready: Option<&Path>) {
    let Ok(entries) = std::fs::read_dir(root.join("tmp").join("pre-encode")) else { return };
    for entry in entries.flatten() {
        if Some(entry.path().as_path()) != ready {
            storage::remove_owned(root, "tmp/pre-encode", &entry.file_name().to_string_lossy());
        }
    }
}

fn wait_idle(mut slot: MutexGuard<'static, Slot>, limit: Duration) -> MutexGuard<'static, Slot> {
    let until = std::time::Instant::now() + limit;
    while slot.running.is_some() {
        let left = until.saturating_duration_since(std::time::Instant::now());
        if left.is_zero() {
            break;
        }
        slot = ENDED.wait_timeout(slot, left).map(|(guard, _)| guard).unwrap_or_else(|p| p.into_inner().0);
    }
    slot
}

/// Export (io worker): a prepared file for exactly this plan is copied into `Recall\Edited`,
/// waiting first for one still running; anything else is stopped and a normal export runs.
pub fn export(info: &ClipInfo, plan: &EditPlan, fps: u32, settings: &ExportSettings) -> Result<ExportSummary, String> {
    let _queued = backend::export_queued();
    let wanted = key(info, plan, fps, settings);
    if let Some(prepared) = wanted.and_then(|key| take_ready(key, backend::export_cancelled)) {
        if source_unchanged(info) {
            let delivered = deliver(&prepared, info, fps);
            let _ = std::fs::remove_file(&prepared.path);
            if delivered.is_ok() {
                return delivered;
            }
        } else {
            let _ = std::fs::remove_file(&prepared.path);
        }
    }
    if backend::export_cancelled() {
        // Cancel pressed while it waited for the pre-encode: the prepared file stays for next time.
        return Err(backend::EXPORT_CANCELLED.to_owned());
    }
    cancel_other(None);
    drop(wait_idle(slot(), Duration::from_secs(30)));
    backend::export_clip(info, plan, fps, settings)
}

/// The prepared file for `key`, waiting while it is still being made (until `stop` says so).
fn take_ready(key: u64, stop: fn() -> bool) -> Option<Prepared> {
    let mut slot = slot();
    while slot.running == Some(key) && !stop() {
        slot = wait_idle(slot, Duration::from_millis(100));
    }
    if stop() {
        return None;
    }
    if slot.ready.as_ref().is_some_and(|ready| ready.key == key) { slot.ready.take() } else { None }
}

/// A clip moved or rewritten since the pre-encode (the SSD keeper, a re-save) is a mismatch.
fn source_unchanged(info: &ClipInfo) -> bool {
    std::fs::metadata(&info.path).is_ok_and(|meta| meta.len() == info.source_bytes)
}

/// Copies the prepared file into the export folder under a hidden name, then renames it.
fn deliver(prepared: &Prepared, info: &ClipInfo, fps: u32) -> Result<ExportSummary, String> {
    let target = backend::export_target(info, fps, false)?;
    let name = target.file_name().and_then(|n| n.to_str()).unwrap_or("export.mp4").to_owned();
    let hidden = target.with_file_name(format!(".{name}{DELIVERING}"));
    let outdir = target.parent().map(Path::to_path_buf).unwrap_or_default();
    let (from, to, temp) = (prepared.path.clone(), target.clone(), hidden.clone());
    let copied = storage::run_io(&outdir, Priority::Interactive, move || -> Result<(), String> {
        std::fs::copy(&from, &temp).map_err(|e| format!("copy prepared export: {e}"))?;
        std::fs::rename(&temp, &to).map_err(|e| format!("publish prepared export: {e}"))
    })?;
    if let Err(error) = copied {
        let _ = std::fs::remove_file(&hidden);
        return Err(error);
    }
    let bytes = std::fs::metadata(&target).map(|m| m.len()).unwrap_or(prepared.summary.bytes);
    Ok(ExportSummary { path: target.to_string_lossy().into_owned(), bytes, ..prepared.summary.clone() })
}

/// Tests that drive the shared slot take this first, so they never see each other's state.
#[cfg(test)]
pub(crate) fn test_slot_lock() -> MutexGuard<'static, ()> {
    static LOCK: Mutex<()> = Mutex::new(());
    LOCK.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

#[cfg(test)]
#[path = "pre_encode_tests.rs"]
mod tests;
