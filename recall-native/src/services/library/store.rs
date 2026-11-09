//! `MetaStore`: `{ "<clip key>": {favourite, title, duration{bytes,mtime,secs}, played} }`
//! at `%APPDATA%\Recall\library.json` (`RECALL_LIBRARY_META` overrides the path). The key is
//! the clip's full path, lowercase ([`clip_key`]); older files keyed by file name are moved to
//! path keys by [`MetaStore::migrate_names`] when the clips are listed.
use std::collections::BTreeMap;
use std::ffi::OsString;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::{store_damage, MetaStore};
use crate::services::durable;

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub(super) struct ClipMeta {
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub favourite: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duration: Option<CachedDuration>,
    /// When the clip was last opened (Unix seconds).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub played: Option<u64>,
    /// Neither the MP4 index nor ffprobe could read this file's length: not probed again on the
    /// next start while the file keeps this size and mtime.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub no_duration: Option<FileStamp>,
    /// Fields a newer build wrote for this clip: kept as they are.
    #[serde(flatten)]
    pub extra: serde_json::Map<String, Value>,
}

impl ClipMeta {
    fn is_empty(&self) -> bool {
        !self.favourite && self.title.is_none() && self.duration.is_none() && self.played.is_none() && self.no_duration.is_none() && self.extra.is_empty()
    }

    /// Folds an old file-name entry into this path entry: a field set on either side survives
    /// (the path entry wins where both are set; the later "last played" wins).
    fn merge_from(&mut self, name: &ClipMeta) {
        self.favourite |= name.favourite;
        if self.title.is_none() {
            self.title.clone_from(&name.title);
        }
        self.duration = self.duration.or(name.duration);
        self.played = self.played.max(name.played);
        self.no_duration = self.no_duration.or(name.no_duration);
    }
}

/// A file's size and mtime, the identity a cached probe result is valid for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub(super) struct FileStamp {
    pub bytes: u64,
    pub mtime: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub(super) struct CachedDuration {
    pub bytes: u64,
    pub mtime: u64,
    pub secs: f64,
}

/// Explicit override, else `%APPDATA%\Recall\library.json`.
pub(super) fn meta_path(override_path: Option<OsString>, appdata: Option<OsString>) -> Option<PathBuf> {
    if let Some(path) = override_path.filter(|p| !p.is_empty()) {
        return Some(PathBuf::from(path));
    }
    appdata.filter(|p| !p.is_empty()).map(|root| crate::user_dirs::checked(PathBuf::from(root).join("Recall").join("library.json")))
}

/// The store key for a clip: its full path, lowercase, backslashes, no `\?\` prefix. Two
/// same-named clips in different folders keep their own favourite, title and last played.
pub fn clip_key(path: &str) -> String {
    let plain = crate::services::storage::plain_path(std::path::Path::new(path));
    super::folders::folder_key(&plain.to_string_lossy())
}

/// Entries with the wrong shape are dropped one by one; None when the file is not a JSON object.
fn parse_clips(bytes: &[u8]) -> Option<BTreeMap<String, ClipMeta>> {
    let Value::Object(map) = serde_json::from_slice::<Value>(bytes).ok()? else { return None };
    Some(map.into_iter().filter_map(|(name, value)| serde_json::from_value::<ClipMeta>(value).ok().map(|meta| (name, meta))).filter(|(_, meta)| !meta.is_empty()).collect())
}

impl MetaStore {
    pub fn load() -> Self {
        match meta_path(std::env::var_os("RECALL_LIBRARY_META"), crate::user_dirs::appdata().map(Into::into)) {
            Some(path) => Self::load_from(path),
            None => Self::default(),
        }
    }

    /// Missing file → empty store. Corrupt file → kept as a dated `.bad`, empty store that
    /// holds background writes until "Keep going" on the damage notice (`store_damage`;
    /// the hold also opens on a later start while the damage is unconfirmed).
    /// Unreadable file (locked, denied) → in-memory store that refuses to overwrite it.
    pub(crate) fn load_from(path: PathBuf) -> Self {
        let now = std::time::SystemTime::now();
        durable::sweep_temps(&path, now);
        let store = match std::fs::read(&path) {
            Ok(bytes) => match parse_clips(&bytes) {
                Some(clips) => Self { path: Some(path), clips, ..Self::default() },
                None => {
                    let Some(backup) = durable::quarantine(&path, now, durable::Keep::Move) else {
                        return Self { write_block: Some("library metadata is damaged and could not be backed up; changes are not saved".into()), ..Self::default() };
                    };
                    // Best effort: without it the hold still opens while the .bad is newer than
                    // library.json, and the first edit writes it again.
                    let _ = store_damage::set_hold(&path, Some(&backup));
                    Self { path: Some(path), ..Self::default() }
                }
            },
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Self { path: Some(path), ..Self::default() },
            Err(error) => return Self { write_block: Some(format!("library metadata unreadable ({error}); changes are not saved")), ..Self::default() },
        };
        store_damage::check(store)
    }

    #[cfg(test)]
    pub(super) fn meta(&self, key: &str) -> Option<&ClipMeta> {
        self.clips.get(key)
    }

    /// The clip's entry: its path key, else an old file-name key not moved yet.
    pub(super) fn meta_of(&self, path: &str, file_name: &str) -> Option<&ClipMeta> {
        self.clips.get(&clip_key(path)).or_else(|| self.clips.get(file_name))
    }

    /// One clip (tests; the library sets favourites through [`Self::set_favourites`]).
    #[cfg(test)]
    pub fn set_favourite(&mut self, key: &str, favourite: bool) -> Result<(), String> {
        self.edit(key, |meta| meta.favourite = favourite)
    }

    /// `None` or empty clears the custom title.
    pub fn set_title(&mut self, file_name: &str, title: Option<&str>) -> Result<(), String> {
        let title = title.map(str::trim).filter(|t| !t.is_empty()).map(str::to_owned);
        self.edit(file_name, |meta| meta.title = title)
    }

    /// Marks the clip as opened at `secs` (Unix seconds).
    /// Not a user change: held in memory while the damage notice is open.
    pub fn set_played(&mut self, file_name: &str, secs: u64) -> Result<(), String> {
        if self.damage.is_some() {
            self.clips.entry(file_name.to_owned()).or_default().played = Some(secs);
            self.dirty = true;
            return Ok(());
        }
        self.edit(file_name, |meta| meta.played = Some(secs))
    }

    /// Favourite on or off for many clips at once: one write, and nothing changes when it fails.
    pub fn set_favourites(&mut self, keys: &[String], favourite: bool) -> Result<(), String> {
        let previous: Vec<(String, Option<ClipMeta>)> = keys.iter().map(|key| (key.clone(), self.clips.get(key).cloned())).collect();
        for key in keys {
            let meta = self.clips.entry(key.clone()).or_default();
            meta.favourite = favourite;
            if meta.is_empty() {
                self.clips.remove(key);
            }
        }
        if let Err(error) = self.write_user_edit() {
            for (key, meta) in previous {
                match meta {
                    Some(meta) => self.clips.insert(key, meta),
                    None => self.clips.remove(&key),
                };
            }
            return Err(error);
        }
        self.dirty = false;
        Ok(())
    }

    /// Moves entries still keyed by file name onto the listed clips' path keys. A name that
    /// several listed clips share goes to each of them once; a name no listed clip has stays
    /// (its folder may be offline). True when anything moved (flush to keep it).
    pub fn migrate_names<'a>(&mut self, clips: impl IntoIterator<Item = (&'a str, &'a str)>) -> bool {
        let mut by_name: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
        for (path, name) in clips {
            if self.clips.contains_key(name) {
                by_name.entry(name).or_default().push(path);
            }
        }
        for (name, paths) in &by_name {
            let Some(meta) = self.clips.remove(*name) else { continue };
            for path in paths {
                self.clips.entry(clip_key(path)).or_default().merge_from(&meta);
            }
        }
        let moved = !by_name.is_empty();
        self.dirty |= moved;
        moved
    }

    /// Cached duration keyed by (clip, bytes, mtime).
    pub fn duration(&self, path: &str, file_name: &str, bytes: u64, mtime: u64) -> Option<f64> {
        let cached = self.meta_of(path, file_name)?.duration?;
        (cached.bytes == bytes && cached.mtime == mtime).then_some(cached.secs)
    }

    pub fn set_duration(&mut self, key: &str, bytes: u64, mtime: u64, secs: f64) {
        if !secs.is_finite() || secs <= 0.0 {
            return;
        }
        let next = Some(CachedDuration { bytes, mtime, secs });
        let meta = self.clips.entry(key.to_owned()).or_default();
        if meta.duration != next || meta.no_duration.is_some() {
            meta.duration = next;
            meta.no_duration = None;
            self.dirty = true;
        }
    }

    /// Remembers that the clip's length could not be read at this size and mtime.
    pub fn set_duration_unknown(&mut self, key: &str, bytes: u64, mtime: u64) {
        let next = Some(FileStamp { bytes, mtime });
        let meta = self.clips.entry(key.to_owned()).or_default();
        if meta.no_duration != next {
            meta.no_duration = next;
            self.dirty = true;
        }
    }

    /// True when an earlier probe of this exact file (same size and mtime) found no length.
    pub fn duration_unknown(&self, path: &str, file_name: &str, bytes: u64, mtime: u64) -> bool {
        self.meta_of(path, file_name).and_then(|m| m.no_duration) == Some(FileStamp { bytes, mtime })
    }

    /// Persists pending duration-cache entries on this thread (tests; the app uses
    /// [`MetaStore::flush_in_background`]).
    #[cfg(test)]
    pub fn flush(&mut self) -> Result<(), String> {
        if !self.dirty || self.damage.is_some() {
            return Ok(());
        }
        self.write()?;
        self.dirty = false;
        Ok(())
    }

    /// [`MetaStore::flush`] with the disk write (and its fsync) on a background thread, so
    /// the UI never waits on the drive. A failed earlier write is reported here and retried.
    pub fn flush_in_background(&mut self) -> Result<(), String> {
        if let Some(error) = self.writer.take_failure() {
            self.dirty = true;
            return Err(error);
        }
        if !self.dirty || self.damage.is_some() {
            return Ok(());
        }
        if let Some(reason) = &self.write_block {
            return Err(reason.clone());
        }
        let Some(path) = self.path.clone() else { return Ok(()) };
        let bytes = serde_json::to_vec(&self.clips).map_err(|error| error.to_string())?;
        let ticket = self.writer.ticket();
        self.writer.write_in_background(ticket, path, bytes);
        self.dirty = false;
        Ok(())
    }

    /// The error of a background flush that failed, once.
    pub fn take_write_failure(&mut self) -> Option<String> {
        let error = self.writer.take_failure()?;
        self.dirty = true;
        Some(error)
    }

    /// Blocks until every background flush is on disk.
    #[cfg(test)]
    pub fn wait_for_writes(&self) {
        self.writer.wait();
    }

    /// Applies a user edit and writes immediately; on a failed write the edit is undone
    /// so the UI never shows a state that is not on disk.
    fn edit(&mut self, file_name: &str, change: impl FnOnce(&mut ClipMeta)) -> Result<(), String> {
        let previous = self.clips.get(file_name).cloned();
        let meta = self.clips.entry(file_name.to_owned()).or_default();
        change(meta);
        if meta.is_empty() {
            self.clips.remove(file_name);
        }
        if let Err(error) = self.write_user_edit() {
            match previous {
                Some(meta) => self.clips.insert(file_name.to_owned(), meta),
                None => self.clips.remove(file_name),
            };
            return Err(error);
        }
        self.dirty = false;
        Ok(())
    }

    /// A star or title edit (one clip or a batch). While the damage notice is open the edit is
    /// saved but the hold stays (on disk too, so a restart or the storage limit can't take the
    /// new file as the whole truth): only "Keep going" ends it.
    fn write_user_edit(&mut self) -> Result<(), String> {
        if let (Some(damage), Some(path)) = (&self.damage, &self.path) {
            store_damage::set_hold(path, damage.backup.as_deref())?;
        }
        self.write()
    }

    /// Every whole-file write goes through here (counted, so a batch can prove it wrote once).
    fn write(&mut self) -> Result<(), String> {
        self.writes += 1;
        self.persist()
    }

    /// Whole-file write to a temp sibling, flushed to disk, then renamed over the original
    /// (in order with the background flushes).
    pub(super) fn persist(&self) -> Result<(), String> {
        if let Some(reason) = &self.write_block {
            return Err(reason.clone());
        }
        let Some(path) = &self.path else { return Ok(()) };
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|error| format!("create library metadata folder: {error}"))?;
        }
        let bytes = serde_json::to_vec(&self.clips).map_err(|error| error.to_string())?;
        let ticket = self.writer.ticket();
        self.writer.write(ticket, path, &bytes).map_err(|error| format!("write library metadata: {error}"))
    }
}

#[cfg(test)]
#[path = "store_tests.rs"]
mod tests;
