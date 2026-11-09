//! A damaged library.json across restarts (deep-audit round 2, R2-1, R2-2).
//!
//! The damage is "open" while `library.json.hold` exists, or while a `library.json.bad*`
//! copy is newer than library.json (or library.json is missing beside one: an older build
//! left it). While open:
//! - favourites and titles still readable in the newest `.bad` are laid into the store
//!   (when it is newer than library.json, i.e. nothing was written back yet);
//! - background writes (durations, played) stay in memory;
//! - a star or title edit is saved but keeps the hold;
//! - the storage cap (controller) does not run.
//!
//! Only "Keep going" on the app's notice ([`MetaStore::keep_going`]) writes the store back
//! and removes the hold.
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use serde_json::Value;

use super::store::ClipMeta;
use super::MetaStore;
use crate::services::{durable, lenient_json};

/// What the library's damage notice shows.
#[derive(Debug, Clone, PartialEq)]
pub struct Damage {
    /// Favourites read back from the damaged copy.
    pub recovered: usize,
    /// The newest damaged copy (`library.json.bad-YYYY-MM-DD[-N]`).
    pub backup: Option<PathBuf>,
}

/// `library.json.hold` (the controller reads the same name).
pub(super) fn hold_path(path: &Path) -> PathBuf {
    let mut name = path.file_name().map(|n| n.to_os_string()).unwrap_or_default();
    name.push(".hold");
    path.with_file_name(name)
}

/// Keeps the hold on disk, so a restart or a later write can't end it.
pub(super) fn set_hold(path: &Path, backup: Option<&Path>) -> Result<(), String> {
    let hold = hold_path(path);
    if hold.exists() {
        return Ok(());
    }
    let note = backup.and_then(Path::file_name).map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    durable::write_durable(&hold, note.as_bytes()).map_err(|e| format!("keep the library hold: {e}"))
}

pub(super) fn clear_hold(path: &Path) -> Result<(), String> {
    match std::fs::remove_file(hold_path(path)) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(format!("clear the library hold: {e}")),
    }
}

/// The newest `<name>.bad*` beside `path`, by modified time.
fn newest_bad(path: &Path) -> Option<(PathBuf, SystemTime)> {
    let prefix = format!("{}.bad", path.file_name()?.to_string_lossy());
    std::fs::read_dir(path.parent()?)
        .ok()?
        .flatten()
        .filter(|e| e.file_name().to_string_lossy().starts_with(&prefix))
        .filter_map(|e| Some((e.path(), e.metadata().ok()?.modified().ok()?)))
        .max_by_key(|(_, at)| *at)
}

/// Favourites and titles in what still parses of a damaged copy.
pub(super) fn salvage(bytes: &[u8]) -> BTreeMap<String, ClipMeta> {
    lenient_json::partial_object(bytes)
        .into_iter()
        .filter_map(|(name, value)| {
            let favourite = value.get("favourite").and_then(Value::as_bool) == Some(true);
            let title = value.get("title").and_then(Value::as_str).map(str::trim).filter(|t| !t.is_empty()).map(str::to_owned);
            (favourite || title.is_some()).then(|| (name, ClipMeta { favourite, title, ..ClipMeta::default() }))
        })
        .collect()
}

/// Lays `salvaged` under `clips`: a star is added, a title only where none is set.
fn merge(clips: &mut BTreeMap<String, ClipMeta>, salvaged: BTreeMap<String, ClipMeta>) {
    for (name, found) in salvaged {
        let meta = clips.entry(name).or_default();
        meta.favourite |= found.favourite;
        if meta.title.is_none() {
            meta.title = found.title;
        }
    }
}

/// Opens the damage on a freshly loaded store when it is still open on disk.
pub(super) fn check(mut store: MetaStore) -> MetaStore {
    let Some(path) = store.path.clone() else { return store };
    let lib_at = std::fs::metadata(&path).and_then(|m| m.modified()).ok();
    let newest = newest_bad(&path);
    let bad_newer = match (&newest, lib_at) {
        (Some((_, bad_at)), Some(lib_at)) => *bad_at > lib_at,
        (Some(_), None) => true,
        (None, _) => false,
    };
    if !bad_newer && !hold_path(&path).exists() {
        return store;
    }
    let salvaged = newest.as_ref().and_then(|(bad, _)| std::fs::read(bad).ok()).map(|bytes| salvage(&bytes)).unwrap_or_default();
    let recovered = salvaged.values().filter(|m| m.favourite).count();
    if bad_newer {
        merge(&mut store.clips, salvaged);
    }
    store.damage = Some(Damage { recovered, backup: newest.map(|(bad, _)| bad) });
    store
}

impl MetaStore {
    /// The damage notice to show, while the library data is held after damage.
    pub fn damage(&self) -> Option<&Damage> {
        self.damage.as_ref()
    }

    /// "Keep going" on the damage notice: writes the store (recovered favourites included)
    /// and removes the hold, so background writes and the storage cap resume.
    pub fn keep_going(&mut self) -> Result<(), String> {
        let Some(path) = self.path.clone() else {
            self.damage = None;
            return Ok(());
        };
        if self.damage.is_none() {
            return Ok(());
        }
        self.persist()?;
        clear_hold(&path)?;
        self.damage = None;
        self.dirty = false;
        Ok(())
    }
}
