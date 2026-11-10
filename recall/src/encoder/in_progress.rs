//! The recorder's own list of the output files it is writing: every long recording's `.part`
//! and every replay save's `.part`, by absolute path. An entry is added before the file is
//! created and removed once it is renamed into place (or kept, or recycled). At the next start
//! recovery works through exactly these paths, wherever they are (a game folder, a clip folder
//! the user has since changed, the staging folder), so nothing it wrote is orphaned.
//!
//! - File: `%APPDATA%\Recall\in-progress.json` (`user_dirs::appdata`, a temp root in unit tests;
//!   every path passes `user_dirs::checked`). Written durably: a temp sibling, flushed, renamed.
//! - Long recordings also keep their bookmarks here (`marks`, capture µs, with `base_us`), so a
//!   crash keeps them, and a repair's undo record (`undo`, `longrec::restore`).
//! - Replay saves keep their final clip path and their packet index (`replay_index`).
//! - [`legacy_scanned`]: the one-time scan for `.part` files from builds before this list
//!   (`longrec::recover`) has run.
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde::{Deserialize, Serialize};

const FILE: &str = "in-progress.json";
const LEGACY_MARKER: &str = "in-progress.legacy-scanned";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Kind {
    Long,
    Replay,
}

/// A repair in progress (`longrec::restore`): enough to put the original bytes back.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Undo {
    /// The file's length before the repair, and where it was cut.
    pub len: u64,
    pub keep: u64,
    /// Where the `mvhd` duration field sits and its bytes before the patch (hex).
    pub mvhd_at: u64,
    pub mvhd_old: String,
    /// The cut-off bytes, saved beside the file, and their FNV-1a 64 hash (hex).
    pub tail: String,
    pub tail_hash: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Entry {
    /// The file being written (its `.part`), absolute.
    pub path: String,
    pub kind: Kind,
    /// Replay: the clip's final path.
    #[serde(default)]
    pub clip: Option<String>,
    /// Replay: the packet index's base path (`replay_index`).
    #[serde(default)]
    pub index: Option<String>,
    /// Long: the capture µs of the recording's first frame, and its bookmarks (capture µs).
    #[serde(default)]
    pub base_us: Option<u64>,
    #[serde(default)]
    pub marks: Vec<u64>,
    #[serde(default)]
    pub undo: Option<Undo>,
}

impl Entry {
    pub(crate) fn long(part: &Path) -> Entry {
        Entry { path: absolute(part), kind: Kind::Long, clip: None, index: None, base_us: None, marks: Vec::new(), undo: None }
    }
    pub(crate) fn replay(part: &Path, clip: &Path, index: Option<&Path>) -> Entry {
        Entry { clip: Some(absolute(clip)), index: index.map(absolute), kind: Kind::Replay, ..Entry::long(part) }
    }
    pub(crate) fn part(&self) -> PathBuf {
        PathBuf::from(&self.path)
    }
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct List {
    version: u32,
    files: Vec<Entry>,
}

static LOCK: Mutex<()> = Mutex::new(());

fn absolute(path: &Path) -> String {
    std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf()).to_string_lossy().into_owned()
}

fn key(path: &str) -> String {
    path.replace('/', "\\").to_lowercase()
}

/// Whether `entry` is the one for `part`.
fn is(entry: &Entry, part: &Path) -> bool {
    key(&entry.path) == key(&absolute(part))
}

/// `%APPDATA%\Recall\in-progress.json`.
pub(crate) fn path() -> Option<PathBuf> {
    Some(crate::user_dirs::checked(crate::user_dirs::appdata()?.join("Recall").join(FILE)))
}

fn load(path: &Path) -> List {
    let Ok(bytes) = std::fs::read(path) else { return List::default() };
    match serde_json::from_slice(&bytes) {
        Ok(list) => list,
        Err(e) => {
            // Never overwrite what could not be read: set it aside, start a fresh list.
            let bad = path.with_extension("json.bad");
            let _ = std::fs::rename(path, &bad);
            eprintln!("[recall] in-progress list {} unreadable ({e}); kept as {}", path.display(), bad.display());
            List::default()
        }
    }
}

/// A temp sibling, flushed to disk, then renamed over `path`.
fn write_durable(path: &Path, bytes: &[u8]) -> Result<(), String> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("create {}: {e}", dir.display()))?;
    }
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let tmp = path.with_file_name(format!("{name}.{}.tmp", std::process::id()));
    std::fs::File::create(&tmp)
        .and_then(|mut f| f.write_all(bytes).and_then(|()| f.sync_all()))
        .map_err(|e| format!("write {}: {e}", tmp.display()))?;
    std::fs::rename(&tmp, path).map_err(|e| format!("rename {} -> {}: {e}", tmp.display(), path.display()))
}

/// Change the list under the lock and write it back when `change` returns true.
fn edit<R>(change: impl FnOnce(&mut Vec<Entry>) -> (bool, R)) -> Result<R, String> {
    let path = path().ok_or("no app-data folder for the in-progress list")?;
    let _lock = LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let mut list = load(&path);
    let (changed, out) = change(&mut list.files);
    if changed {
        list.version = 1;
        let bytes = serde_json::to_vec_pretty(&list).map_err(|e| e.to_string())?;
        write_durable(&path, &bytes)?;
    }
    Ok(out)
}

/// Add `entry` (replacing one for the same file).
pub(crate) fn add(entry: Entry) -> Result<(), String> {
    edit(|files| {
        files.retain(|e| key(&e.path) != key(&entry.path));
        files.push(entry);
        (true, ())
    })
}

/// Remove the entry for `part` (none: nothing to write).
pub(crate) fn remove(part: &Path) -> Result<(), String> {
    edit(|files| {
        let before = files.len();
        files.retain(|e| !is(e, part));
        (files.len() != before, ())
    })
}

/// Change the entry for `part`; `false` when there is none.
pub(crate) fn update(part: &Path, change: impl FnOnce(&mut Entry)) -> Result<bool, String> {
    edit(|files| match files.iter_mut().find(|e| is(e, part)) {
        Some(entry) => {
            change(entry);
            (true, true)
        }
        None => (false, false),
    })
}

pub(crate) fn get(part: &Path) -> Option<Entry> {
    list().into_iter().find(|e| is(e, part))
}

pub(crate) fn list() -> Vec<Entry> {
    let Some(path) = path() else { return Vec::new() };
    let _lock = LOCK.lock().unwrap_or_else(|p| p.into_inner());
    load(&path).files
}

fn legacy_marker() -> Option<PathBuf> {
    Some(crate::user_dirs::checked(crate::user_dirs::appdata()?.join("Recall").join(LEGACY_MARKER)))
}

/// The one-time scan for pre-list `.part` files has run.
pub(crate) fn legacy_scanned() -> bool {
    legacy_marker().is_some_and(|p| p.is_file())
}

pub(crate) fn mark_legacy_scanned() -> Result<(), String> {
    let path = legacy_marker().ok_or("no app-data folder")?;
    write_durable(&path, b"1")
}

/// FNV-1a 64 of `bytes`, as hex (the undo record's check on its saved tail).
pub(crate) fn fnv_hex(bytes: &[u8]) -> String {
    let hash = bytes.iter().fold(0xcbf2_9ce4_8422_2325u64, |h, b| (h ^ u64::from(*b)).wrapping_mul(0x0100_0000_01b3));
    format!("{hash:016x}")
}
