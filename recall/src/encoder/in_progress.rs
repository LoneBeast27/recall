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
    /// Replay: rebuilt into `rebuilt`, but the `.part` holds more than the index knew when it
    /// was `held_more` bytes long: kept listed, rebuilt again only once the index has grown.
    #[serde(default)]
    pub held_more: Option<u64>,
    #[serde(default)]
    pub rebuilt: Option<String>,
}

impl Entry {
    pub(crate) fn long(part: &Path) -> Entry {
        Entry { path: absolute(part), kind: Kind::Long, clip: None, index: None, base_us: None, marks: Vec::new(), undo: None, held_more: None, rebuilt: None }
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

/// A path's comparison key: absolute, backslashes, lowercase (`/` and `\` name the same file).
pub(crate) fn key(path: &str) -> String {
    absolute(Path::new(path)).replace('/', "\\").trim_end_matches('\\').to_lowercase()
}

/// Whether `a` and `b` name the same file (see [`key`]).
pub(crate) fn same_path(a: &str, b: &str) -> bool {
    key(a) == key(b)
}

/// Whether `entry` is the one for `part`.
fn is(entry: &Entry, part: &Path) -> bool {
    same_path(&entry.path, &part.to_string_lossy())
}

/// `%APPDATA%\Recall\in-progress.json`.
pub(crate) fn path() -> Option<PathBuf> {
    Some(crate::user_dirs::checked(crate::user_dirs::appdata()?.join("Recall").join(FILE)))
}

/// The list on disk: none yet is empty; any other read error is an error (never an empty list
/// that a write would then put over the entries it could not read).
fn load(path: &Path) -> Result<List, String> {
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(List::default()),
        Err(e) => return Err(format!("read {}: {e}", path.display())),
    };
    match serde_json::from_slice(&bytes) {
        Ok(list) => Ok(list),
        Err(e) => {
            // Never overwrite what could not be parsed: set it aside first, then start afresh.
            let bad = path.with_extension("json.bad");
            std::fs::rename(path, &bad).map_err(|r| format!("{} unparsable ({e}) and not set aside: {r}", path.display()))?;
            eprintln!("[recall] in-progress list {} unparsable ({e}); kept as {}", path.display(), bad.display());
            Ok(List::default())
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
    let mut list = load(&path)?;
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

/// The entries, or why the list cannot be read now (its entries stay on disk).
pub(crate) fn try_list() -> Result<Vec<Entry>, String> {
    let path = path().ok_or("no app-data folder for the in-progress list")?;
    let _lock = LOCK.lock().unwrap_or_else(|p| p.into_inner());
    load(&path).map(|list| list.files)
}

/// The entries (none when the list cannot be read now: they stay on disk for the next start).
pub(crate) fn list() -> Vec<Entry> {
    try_list().unwrap_or_else(|e| {
        eprintln!("[recall] in-progress list not read: {e}");
        Vec::new()
    })
}

fn legacy_marker() -> Option<PathBuf> {
    Some(crate::user_dirs::checked(crate::user_dirs::appdata()?.join("Recall").join(LEGACY_MARKER)))
}

/// The one-time scan for pre-list `.part` files has run (and no part since asked for it again).
pub(crate) fn legacy_scanned() -> bool {
    legacy_marker().and_then(|p| std::fs::read(p).ok()).is_some_and(|b| b == b"1")
}

/// A part could not be listed: the next start runs the one-time scan again, so a crash never
/// leaves it to nobody (the scan finds it in the clip folder).
pub(crate) fn request_legacy_scan() -> Result<(), String> {
    let path = legacy_marker().ok_or("no app-data folder")?;
    write_durable(&path, b"rescan")
}

pub(crate) fn mark_legacy_scanned() -> Result<(), String> {
    let path = legacy_marker().ok_or("no app-data folder")?;
    write_durable(&path, b"1")
}

/// FNV-1a 64's start value; [`fnv_update`] folds bytes in (the undo record's check on its tail).
pub(crate) const FNV_START: u64 = 0xcbf2_9ce4_8422_2325;

pub(crate) fn fnv_update(hash: u64, bytes: &[u8]) -> u64 {
    bytes.iter().fold(hash, |h, b| (h ^ u64::from(*b)).wrapping_mul(0x0100_0000_01b3))
}

/// FNV-1a 64 of `bytes`, as hex.
#[cfg(test)]
pub(crate) fn fnv_hex(bytes: &[u8]) -> String {
    format!("{:016x}", fnv_update(FNV_START, bytes))
}
