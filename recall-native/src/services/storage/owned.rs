//! Which cache files Recall made. Eviction and every sweep delete a file in `video\`, `light\`
//! or `tmp\pre-encode\` only when its name follows Recall's scheme AND `owned.json` lists it: a
//! clip the user (or a mistaken folder pick) put there is counted, never deleted.
//!
//! * Names: `<id>.mp4`, `<id>` 16 lowercase hex digits ([`super::identity`], the pre-encode
//!   key); a copy in flight `<id>.mp4.<pid>-<nanos>.part` ([`super::temp_sibling`]); the
//!   engine's export temp `.<id>.recall-export-<pid>-<nonce>-<n>.mp4`.
//! * `owned.json` at the cache root lists `<folder>/<id>.mp4`. A target is listed BEFORE its
//!   file is created, so the part a crash cut off is owned through its target; an entry goes
//!   when Recall deleted the file and no temporary of it is left ([`forget`]).
//! * A cache from before the list is adopted once, and only when `owned.json` is truly missing
//!   (and none was ever set aside): only files with the exact names above join it, and
//!   `since_ms` records when. A user's clip carries its own name (`VALORANT__2026-...mp4`), so
//!   it stays out. A cache the guard blocked adopts nothing.
//! * A list that exists but cannot be read is never re-adopted over: a corrupt one is set aside
//!   as `owned.json.corrupt-<unix secs>-<n>` (a counter, so two in one second never overwrite
//!   each other) and a locked one is left as it is; in both cases this pass owns nothing, so
//!   nothing is evicted. The newest three set-aside lists are kept; older ones are the only
//!   files this module ever removes outside the cache tiers, and only by that exact name.
//! * Once a valid list exists again (the next copy Recall makes starts one), the files the
//!   set-aside left unlisted are adopted once more, by the same exact-name rule as a first
//!   adoption (`readopted` records it), so they do not leak.
//!
//! Why a list and not a per-file marker: the cache may sit on a volume without alternate
//! streams, and one small file read per inventory is cheaper than one open per cache file.
use std::collections::BTreeSet;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde::{Deserialize, Serialize};

const FILE: &str = "owned.json";
const CORRUPT: &str = "owned.json.corrupt-";
/// Folders whose files are listed.
const FOLDERS: [&str; 3] = ["video", "light", "tmp/pre-encode"];
/// Serializes read-modify-writes of `owned.json`. Never taken around [`super::budget::INDEX_LOCK`].
static LOCK: Mutex<()> = Mutex::new(());

#[derive(Default, Serialize, Deserialize)]
#[serde(default)]
struct Owned {
    files: BTreeSet<String>,
    /// When this cache was adopted (Unix ms): kept copies older than it predate the engine's mark.
    since_ms: Option<u64>,
    /// The files a set-aside left unlisted were adopted again into this list.
    readopted: bool,
}

/// What reading `owned.json` found.
enum Read {
    Listed(Owned),
    /// No list, and none was ever set aside: a cache from before the list.
    Missing,
    /// There but unreadable (corrupt, locked, denied), or one was set aside before.
    Unreadable(String),
}

/// 16 lowercase hex digits (`format!("{hash:016x}")`).
pub(super) fn is_id(text: &str) -> bool {
    text.len() == 16 && text.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// `<id>.mp4` → its id.
pub(super) fn recall_mp4(name: &str) -> Option<&str> {
    name.strip_suffix(".mp4").filter(|id| is_id(id))
}

/// The `<id>.mp4` a Recall-made temporary belongs to (see the module doc), else `None`.
pub(super) fn temp_target(name: &str) -> Option<String> {
    let digits = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit());
    if let Some(rest) = name.strip_suffix(".part") {
        let (id, nonce) = rest.split_once(".mp4.")?;
        let (pid, nanos) = nonce.split_once('-')?;
        return (is_id(id) && digits(pid) && digits(nanos)).then(|| format!("{id}.mp4"));
    }
    let (id, tail) = name.strip_prefix('.')?.split_once(".recall-export-")?;
    (is_id(id) && tail.ends_with(".mp4")).then(|| format!("{id}.mp4"))
}

/// `<id>.mp4` itself or the target of a temporary.
fn target_of(name: &str) -> Option<String> {
    if recall_mp4(name).is_some() { Some(name.to_owned()) } else { temp_target(name) }
}

fn lock() -> std::sync::MutexGuard<'static, ()> {
    LOCK.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// `owned.json.corrupt-<secs>` or `owned.json.corrupt-<secs>-<n>` → (secs, n; -1 without one).
fn set_aside_key(name: &str) -> Option<(u64, i64)> {
    let rest = name.strip_prefix(CORRUPT)?;
    let digits = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit());
    match rest.split_once('-') {
        Some((secs, n)) if digits(secs) && digits(n) => Some((secs.parse().ok()?, n.parse().ok()?)),
        None if digits(rest) => Some((rest.parse().ok()?, -1)),
        _ => None,
    }
}

/// The set-aside lists directly in `root` (regular files with the exact name), oldest first.
fn set_aside_lists(root: &Path) -> Vec<((u64, i64), PathBuf)> {
    let Ok(listing) = std::fs::read_dir(root) else { return Vec::new() };
    let mut lists: Vec<((u64, i64), PathBuf)> = listing
        .flatten()
        .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_file()))
        .filter_map(|entry| Some((set_aside_key(&entry.file_name().to_string_lossy())?, entry.path())))
        .collect();
    lists.sort();
    lists
}

fn set_aside_before(root: &Path) -> bool {
    !set_aside_lists(root).is_empty()
}

/// Set-aside lists kept for a look; older ones go.
const KEEP_SET_ASIDE: usize = 3;

fn read(root: &Path) -> Read {
    match std::fs::read(root.join(FILE)) {
        Ok(bytes) => match serde_json::from_slice(&bytes) {
            Ok(owned) => Read::Listed(owned),
            Err(error) => Read::Unreadable(format!("corrupt: {error}")),
        },
        Err(error) if error.kind() == std::io::ErrorKind::NotFound && !set_aside_before(root) => Read::Missing,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Read::Unreadable("an earlier list was set aside".into()),
        Err(error) => Read::Unreadable(format!("unreadable: {error}")),
    }
}

/// Writes the list to a temp sibling, flushes it to disk, then renames it into place.
fn write(root: &Path, owned: &Owned) {
    let path = root.join(FILE);
    let temporary = super::temp_sibling(&path);
    let saved = serde_json::to_vec(owned).map_err(|error| error.to_string()).and_then(|bytes| {
        let mut file = std::fs::File::create(&temporary).map_err(|error| error.to_string())?;
        file.write_all(&bytes).and_then(|()| file.sync_all()).map_err(|error| error.to_string())?;
        drop(file);
        std::fs::rename(&temporary, &path).map_err(|error| error.to_string())
    });
    if let Err(error) = saved {
        let _ = std::fs::remove_file(&temporary);
        eprintln!("save cache ownership list: {error}");
    }
}

/// Renames a corrupt list to `owned.json.corrupt-<unix secs>-<n>` (the first free `n`); a locked
/// one cannot move and stays. Then only the newest [`KEEP_SET_ASIDE`] set-aside lists remain.
fn set_aside(root: &Path, why: &str) {
    let secs = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs());
    // After the newest of this second: a free counter freed by pruning would sort as the oldest.
    let next = set_aside_lists(root).iter().filter(|((at, _), _)| *at == secs).map(|((_, n), _)| n + 1).max().unwrap_or(0);
    let Some(aside) = (next..next + 1000).map(|n| root.join(format!("{CORRUPT}{secs}-{n}"))).find(|path| !path.exists()) else { return };
    match std::fs::rename(root.join(FILE), &aside) {
        Ok(()) => eprintln!("cache ownership list {why}; set aside as {}, nothing adopted", aside.display()),
        Err(error) => eprintln!("cache ownership list {why}; left in place ({error}), nothing adopted"),
    }
    let lists = set_aside_lists(root);
    for (_, old) in lists.iter().take(lists.len().saturating_sub(KEEP_SET_ASIDE)) {
        let _ = std::fs::remove_file(old);
    }
}

fn now_ms() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_millis() as u64)
}

/// The list; adopts a cache from before it only when it is truly missing (see the module doc).
/// Call with [`LOCK`] held.
fn load_locked(root: &Path) -> Owned {
    match read(root) {
        Read::Listed(mut owned) if !owned.readopted && set_aside_before(root) && super::guard::blocked(root).is_none() => {
            owned.files.extend(legacy(root));
            owned.readopted = true;
            write(root, &owned);
            owned
        }
        Read::Listed(owned) => owned,
        Read::Unreadable(why) => {
            if root.join(FILE).exists() && why.starts_with("corrupt") {
                set_aside(root, &why);
            }
            Owned::default()
        }
        Read::Missing if super::guard::blocked(root).is_some() => Owned::default(),
        Read::Missing => {
            let owned = Owned { files: legacy(root), since_ms: Some(now_ms()), readopted: false };
            write(root, &owned);
            owned
        }
    }
}

/// Recall's exact names in a cache made before the list. Reads only.
fn legacy(root: &Path) -> BTreeSet<String> {
    let mut files = BTreeSet::new();
    for folder in FOLDERS {
        if super::unit_files::is_reparse(&root.join(folder)) {
            eprintln!("cache: {} is a junction or link; nothing adopted from it", root.join(folder).display());
            continue;
        }
        let Ok(listing) = std::fs::read_dir(root.join(folder)) else { continue };
        for entry in listing.flatten().filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_file())) {
            if let Some(target) = target_of(&entry.file_name().to_string_lossy()) {
                files.insert(format!("{folder}/{target}"));
            }
        }
    }
    files
}

/// The dry run's view (writes nothing, sets nothing aside): (what is listed, what a first
/// load would adopt).
// Reached through `storage::plan_dry` (tests and tools; no Settings caller yet).
#[allow(dead_code)]
pub(super) fn peek(root: &Path) -> (BTreeSet<String>, Vec<String>) {
    let _guard = lock();
    match read(root) {
        Read::Listed(owned) => (owned.files, Vec::new()),
        Read::Missing if super::guard::blocked(root).is_none() => {
            let adopt = legacy(root);
            let list = adopt.iter().cloned().collect();
            (adopt, list)
        }
        _ => (BTreeSet::new(), Vec::new()),
    }
}

/// Every listed `<folder>/<id>.mp4`.
pub(super) fn listed(root: &Path) -> BTreeSet<String> {
    let _guard = lock();
    load_locked(root).files
}


/// Lists `rel` (`video/<id>.mp4`): call before the file or its temporary is created. With an
/// unreadable list the mark is not saved (the file is then never evicted: a leak, not a loss).
pub(super) fn mark(root: &Path, rel: &str) {
    let _guard = lock();
    if matches!(read(root), Read::Unreadable(_)) && root.join(FILE).exists() {
        let _ = load_locked(root);
        return;
    }
    let mut owned = load_locked(root);
    if owned.files.insert(rel.to_owned()) {
        write(root, &owned);
    }
}

/// True when `name` in `folder` is a listed `<id>.mp4` or a temporary of one.
pub(super) fn owns(listed: &BTreeSet<String>, folder: &str, name: &str) -> bool {
    target_of(name).is_some_and(|target| listed.contains(&format!("{folder}/{target}")))
}

/// After Recall deleted `name` (a target or a temporary) in `folder`: drops its target from
/// the list once neither the target nor any temporary of it is left.
pub(super) fn forget(root: &Path, folder: &str, name: &str) {
    let Some(target) = target_of(name) else { return };
    let directory: PathBuf = root.join(folder);
    let left = directory.join(&target).exists()
        || std::fs::read_dir(&directory)
            .map(|listing| listing.flatten().any(|entry| temp_target(&entry.file_name().to_string_lossy()).as_deref() == Some(target.as_str())))
            .unwrap_or(false);
    if left {
        return;
    }
    let _guard = lock();
    let Read::Listed(mut owned) = read(root) else { return };
    if owned.files.remove(&format!("{folder}/{target}")) {
        write(root, &owned);
    }
}

/// Deletes `name` in `folder` if Recall made it and the cache is not blocked; true when gone.
pub(super) fn remove(root: &Path, folder: &str, name: &str) -> bool {
    if super::guard::blocked(root).is_some() || !owns(&listed(root), folder, name) {
        return false;
    }
    // A tier folder that is a junction or link leads elsewhere: never delete through it.
    let directory = root.join(folder);
    if super::unit_files::is_reparse(&directory) || directory.parent().is_some_and(super::unit_files::is_reparse) {
        eprintln!("cache: {} is a junction or link; {name} not deleted", directory.display());
        return false;
    }
    let removed = std::fs::remove_file(root.join(folder).join(name)).is_ok();
    if removed {
        forget(root, folder, name);
    }
    removed
}
