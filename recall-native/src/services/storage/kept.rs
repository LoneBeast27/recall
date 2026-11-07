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
//! the engine kept while the app was closed, and deletes a marked copy only once its clip is
//! gone for sure (its folder readable, no clip and no `.part` in it).
//!
//! Only the engine's own copies are ever deleted: it marks each with the alternate stream
//! [`KEPT_MARK`] (`recall/src/encoder/mover.rs`). An unmarked file (a user's, or a copy kept
//! before the mark existed) is adopted when it is the clip's copy by name, size and time,
//! and otherwise left where it is. A cache the guard blocked (`guard.rs`) touches nothing.
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use super::budget;
use super::video::{budget_in, touch, video_dir};

/// NTFS alternate stream the engine writes on each copy it keeps (`recall/src/encoder/mover.rs`,
/// `KEPT_MARK`): the sweep deletes only marked files.
pub(super) const KEPT_MARK: &str = "recall.kept";

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
    if spawned.is_err() && marked(&file) {
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

/// The engine marked `kept` as its own copy.
pub(super) fn marked(kept: &Path) -> bool {
    std::fs::metadata(format!("{}:{KEPT_MARK}", kept.display())).is_ok()
}

/// `kept` is a copy of the clip `dest` by size and modification time (whole seconds, the
/// cache identity): the engine stamps the clip with the staged copy's time.
fn copy_of(dest: &Path, kept: &Path) -> bool {
    matches!((dest.metadata(), kept.metadata()), (Ok(d), Ok(k)) if d.is_file() && d.len() == k.len() && super::mtime_secs(&d) == super::mtime_secs(&k))
}

/// The clip is gone for sure: its folder is there and readable, and neither the clip nor its
/// `<clip>.part` is in it (`try_exists` said no; an error is not a no). Anything else keeps the
/// copy (review RI-2): a folder asleep or unreadable (a NAS, a wrong folder list), a stat
/// error, a clip still on disk, or only a `.part` (a power cut lost the move's rename, the kept
/// copy may be the only whole clip, and the engine moves it again at its next start:
/// `recall/src/encoder/staging_kept.rs`).
fn clip_gone(dest: &Path) -> bool {
    let mut part = dest.as_os_str().to_owned();
    part.push(".part");
    let folder_readable = dest.parent().is_some_and(|folder| std::fs::read_dir(folder).is_ok());
    folder_readable && matches!(dest.try_exists(), Ok(false)) && matches!(Path::new(&part).try_exists(), Ok(false))
}

/// [`sweep_kept`] against explicit folders: a kept copy of a clip in `clip_folder` is
/// adopted; a marked copy is deleted only when its clip is gone for sure ([`clip_gone`]);
/// anything else stays. Returns (adopted, deleted).
pub(super) fn sweep_in(root: &Path, kept_dir: &Path, clip_folder: &Path, budget: u64) -> (usize, usize) {
    if super::guard::blocked(root).or_else(|| super::guard::blocked(kept_dir)).is_some() {
        return (0, 0);
    }
    let Ok(listing) = std::fs::read_dir(kept_dir) else { return (0, 0) };
    let (mut adopted, mut deleted) = (0, 0);
    for entry in listing.flatten() {
        if !entry.file_type().is_ok_and(|t| t.is_file()) {
            continue;
        }
        let kept = entry.path();
        let dest = clip_folder.join(entry.file_name());
        if copy_of(&dest, &kept) && adopt_in(root, &dest, &kept, budget).is_ok() {
            adopted += 1;
        } else if marked(&kept) && clip_gone(&dest) && std::fs::remove_file(&kept).is_ok() {
            deleted += 1;
        }
    }
    // Copies kept before the mark existed stay where they are: one may be the only copy of
    // its clip, and Recall never deletes a clip (review 11-07). They leak; they are not deleted.
    (adopted, deleted)
}

/// Moves `kept` into the video tier as the copy of `dest`. Err when it is not a copy of `dest`
/// by size, the clip is too large for the tier, or the move fails; `kept` is then deleted only
/// when the engine marked it. A blocked cache adopts and deletes nothing.
pub(super) fn adopt_in(root: &Path, dest: &Path, kept: &Path, budget: u64) -> Result<PathBuf, String> {
    if let Some(why) = super::guard::blocked(root).or_else(|| super::guard::blocked(kept)) {
        return Err(format!("kept copy left alone: {why}"));
    }
    let _guard = ADOPT_LOCK.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    let result = adopt_locked(root, dest, kept, budget);
    if result.is_err() && marked(kept) {
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
        if marked(kept) {
            let _ = std::fs::remove_file(kept);
        }
    } else {
        super::owned::mark(root, &format!("video/{file}"));
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
