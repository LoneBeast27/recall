//! Fresh clips straight into the video tier. The engine records to the SSD and moves each
//! clip to the clip folder in the background; after a copy across volumes it keeps the
//! staged copy at `<install folder>\cache\staging\kept\<clip file name>` (same bytes and
//! modification time as the clip) and names it in `clip_moved` as `kept_path`. The app
//! adopts that copy as `video/<id>.mp4`, so a clip saved mid-game opens from the SSD at once,
//! without reading the hard drive the recorder is writing to.
//!
//! Adoption renames on the same volume and copies otherwise, then indexes the file like
//! [`super::video::cache_in`] (LRU touch, budget). A copy whose size differs from the clip, or
//! a clip too large for the video tier, is deleted instead. At start [`sweep_kept`] adopts what
//! the engine kept while the app was closed, and deletes what no longer matches a clip.
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use super::budget;
use super::video::{budget_in, touch, video_dir};

/// One adoption at a time (an engine event and the start-up sweep may name the same file).
static ADOPT_LOCK: Mutex<()> = Mutex::new(());

/// `<folder of the running exe>\cache\staging\kept`: where the engine (installed beside the
/// app) keeps staged copies (`recall/src/encoder/staging.rs`, `default_dir`).
fn engine_kept_dir() -> Option<PathBuf> {
    Some(std::env::current_exe().ok()?.parent()?.join("cache").join("staging").join("kept"))
}

/// The folders a kept copy may come from: the engine's (beside the exe) and the cache's own.
fn kept_dirs() -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = engine_kept_dir().into_iter().collect();
    dirs.push(super::cache_root().join("staging").join("kept"));
    dirs
}

/// True only for a file directly inside one of `dirs`, after both are canonicalized. Everything
/// else (a clip in Raw, an export, a path that walks out with `..`, a missing file) is refused:
/// adoption moves and deletes the file it is given, so it must never be one of the user's clips.
pub(super) fn inside_kept(kept: &Path, dirs: &[PathBuf]) -> bool {
    let Ok(file) = std::fs::canonicalize(kept) else { return false };
    let Some(parent) = file.parent() else { return false };
    file.is_file() && dirs.iter().filter_map(|dir| std::fs::canonicalize(dir).ok()).any(|dir| dir.as_path() == parent)
}

/// Adopts the engine's kept copy of the clip now at `dest`, on its own thread. A path outside
/// the kept folders is logged and ignored: nothing is moved or deleted.
pub fn adopt_kept(dest: String, kept: String) {
    let file = PathBuf::from(&kept);
    if !inside_kept(&file, &kept_dirs()) {
        eprintln!("kept clip ignored: {kept} is outside the kept-copy folders");
        return;
    }
    let spawned = std::thread::Builder::new().name("recall-adopt".into()).spawn(move || {
        let root = super::cache_root();
        let budget = budget_in(&root);
        if let Err(error) = adopt_in(&root, Path::new(&dest), Path::new(&kept), budget) {
            eprintln!("kept clip not cached: {error}");
        }
    });
    if spawned.is_err() {
        let _ = std::fs::remove_file(file);
    }
}

/// Adopts or deletes every kept copy the engine left, on its own thread. `clip_folder` is
/// read there (it may load `folders.json`).
pub fn sweep_kept(clip_folder: impl FnOnce() -> PathBuf + Send + 'static) {
    let Some(kept_dir) = engine_kept_dir().filter(|dir| dir.is_dir()) else { return };
    let _ = std::thread::Builder::new().name("recall-adopt-sweep".into()).spawn(move || {
        let root = super::cache_root();
        let budget = budget_in(&root);
        let (adopted, deleted) = sweep_in(&root, &kept_dir, &clip_folder(), budget);
        if adopted + deleted > 0 {
            eprintln!("kept clips: {adopted} cached, {deleted} deleted");
        }
    });
}

/// [`sweep_kept`] against explicit folders: a kept copy whose clip is in `clip_folder` with the
/// same size is adopted, anything else is deleted. Returns (adopted, deleted).
pub(super) fn sweep_in(root: &Path, kept_dir: &Path, clip_folder: &Path, budget: u64) -> (usize, usize) {
    let Ok(listing) = std::fs::read_dir(kept_dir) else { return (0, 0) };
    let (mut adopted, mut deleted) = (0, 0);
    for entry in listing.flatten() {
        if !entry.file_type().is_ok_and(|t| t.is_file()) {
            continue;
        }
        let kept = entry.path();
        let dest = clip_folder.join(entry.file_name());
        let same_size = matches!((dest.metadata(), entry.metadata()), (Ok(d), Ok(k)) if d.is_file() && d.len() == k.len());
        if same_size && adopt_in(root, &dest, &kept, budget).is_ok() {
            adopted += 1;
        } else {
            let _ = std::fs::remove_file(&kept);
            deleted += 1;
        }
    }
    (adopted, deleted)
}

/// Moves `kept` into the video tier as the copy of `dest`. Err (and `kept` deleted) when it
/// is not a copy of `dest` by size, the clip is too large for the tier, or the move fails.
pub(super) fn adopt_in(root: &Path, dest: &Path, kept: &Path, budget: u64) -> Result<PathBuf, String> {
    let _guard = ADOPT_LOCK.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    let result = adopt_locked(root, dest, kept, budget);
    if result.is_err() {
        let _ = std::fs::remove_file(kept);
    }
    result
}

fn adopt_locked(root: &Path, dest: &Path, kept: &Path, budget: u64) -> Result<PathBuf, String> {
    let (id, bytes, _) = super::identity_of(dest)?;
    let kept_bytes = kept.metadata().map_err(|error| format!("stat {}: {error}", kept.display()))?.len();
    if kept_bytes != bytes {
        return Err(format!("{} is {kept_bytes} bytes, the clip {bytes}", kept.display()));
    }
    if bytes > budget / 2 {
        return Err(format!("clip ({bytes} bytes) is larger than the video tier allows"));
    }
    let directory = video_dir(root);
    std::fs::create_dir_all(&directory).map_err(|error| format!("create video cache: {error}"))?;
    let file = format!("{id}.mp4");
    let target = directory.join(&file);
    if target.metadata().is_ok_and(|m| m.len() == bytes) {
        let _ = std::fs::remove_file(kept);
    } else {
        budget::enforce(root, budget, bytes);
        place(kept, &target)?;
    }
    touch(root, &file);
    Ok(target)
}

/// Rename (same volume), else copy to a temp sibling, publish it and delete `kept`.
fn place(kept: &Path, target: &Path) -> Result<(), String> {
    if std::fs::rename(kept, target).is_ok() {
        return Ok(());
    }
    let temporary = super::temp_sibling(target);
    if let Err(error) = std::fs::copy(kept, &temporary) {
        let _ = std::fs::remove_file(&temporary);
        return Err(format!("copy kept clip: {error}"));
    }
    super::ffmpeg::publish(&temporary, target)?;
    let _ = std::fs::remove_file(kept);
    Ok(())
}

#[cfg(test)]
#[path = "kept_tests.rs"]
mod tests;
