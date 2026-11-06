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
//! * A cache from before the list (no `owned.json`, or an unreadable one) is adopted once:
//!   only files with the exact names above join it. A user's clip carries its own name
//!   (`VALORANT__2026-...mp4`), so it stays out. A cache the guard blocked adopts nothing.
//!
//! Why a list and not a per-file marker: the cache may sit on a volume without alternate
//! streams, and one small file read per inventory is cheaper than one open per cache file.
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde::{Deserialize, Serialize};

const FILE: &str = "owned.json";
/// Folders whose files are listed.
const FOLDERS: [&str; 3] = ["video", "light", "tmp/pre-encode"];
/// Serializes read-modify-writes of `owned.json`. Never taken around [`super::budget::INDEX_LOCK`].
static LOCK: Mutex<()> = Mutex::new(());

#[derive(Default, Serialize, Deserialize)]
#[serde(default)]
struct Owned {
    files: BTreeSet<String>,
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

fn read(root: &Path) -> Option<Owned> {
    serde_json::from_slice(&std::fs::read(root.join(FILE)).ok()?).ok()
}

fn write(root: &Path, owned: &Owned) {
    let saved = serde_json::to_vec(owned).map_err(|error| error.to_string()).and_then(|bytes| super::projects::write_atomic(&root.join(FILE), &bytes));
    if let Err(error) = saved {
        eprintln!("save cache ownership list: {error}");
    }
}

/// The list, adopting a cache from before it once. Call with [`LOCK`] held.
fn load_locked(root: &Path) -> Owned {
    if let Some(owned) = read(root) {
        return owned;
    }
    if super::guard::blocked(root).is_some() {
        return Owned::default();
    }
    let owned = Owned { files: legacy(root) };
    write(root, &owned);
    owned
}

/// Recall's exact names in a cache made before the list.
fn legacy(root: &Path) -> BTreeSet<String> {
    let mut files = BTreeSet::new();
    for folder in FOLDERS {
        let Ok(listing) = std::fs::read_dir(root.join(folder)) else { continue };
        for entry in listing.flatten().filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_file())) {
            if let Some(target) = target_of(&entry.file_name().to_string_lossy()) {
                files.insert(format!("{folder}/{target}"));
            }
        }
    }
    files
}

/// Every listed `<folder>/<id>.mp4`.
pub(super) fn listed(root: &Path) -> BTreeSet<String> {
    let _guard = lock();
    load_locked(root).files
}

/// Lists `rel` (`video/<id>.mp4`): call before the file or its temporary is created.
pub(super) fn mark(root: &Path, rel: &str) {
    let _guard = lock();
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
    let mut owned = load_locked(root);
    if owned.files.remove(&format!("{folder}/{target}")) {
        write(root, &owned);
    }
}

/// Deletes `name` in `folder` if Recall made it and the cache is not blocked; true when gone.
pub(super) fn remove(root: &Path, folder: &str, name: &str) -> bool {
    if super::guard::blocked(root).is_some() || !owns(&listed(root), folder, name) {
        return false;
    }
    let removed = std::fs::remove_file(root.join(folder).join(name)).is_ok();
    if removed {
        forget(root, folder, name);
    }
    removed
}
