//! `MetaStore`: `{ "<file name>": {favourite, title, duration{bytes,mtime,secs}} }`
//! at `%APPDATA%\Recall\library.json` (`RECALL_LIBRARY_META` overrides the path).
use std::collections::BTreeMap;
use std::ffi::OsString;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::MetaStore;

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub(super) struct ClipMeta {
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub favourite: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duration: Option<CachedDuration>,
}

impl ClipMeta {
    fn is_empty(&self) -> bool {
        !self.favourite && self.title.is_none() && self.duration.is_none()
    }
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
    appdata.filter(|p| !p.is_empty()).map(|root| PathBuf::from(root).join("Recall").join("library.json"))
}

/// `library.json` → `library.json.bad`.
pub(super) fn quarantine_path(path: &Path) -> PathBuf {
    let mut name = path.file_name().map(OsString::from).unwrap_or_else(|| OsString::from("library.json"));
    name.push(".bad");
    path.with_file_name(name)
}

/// Entries with the wrong shape are dropped one by one; None when the file is not a JSON object.
fn parse_clips(bytes: &[u8]) -> Option<BTreeMap<String, ClipMeta>> {
    let Value::Object(map) = serde_json::from_slice::<Value>(bytes).ok()? else { return None };
    Some(map.into_iter().filter_map(|(name, value)| serde_json::from_value::<ClipMeta>(value).ok().map(|meta| (name, meta))).filter(|(_, meta)| !meta.is_empty()).collect())
}

impl MetaStore {
    pub fn load() -> Self {
        match meta_path(std::env::var_os("RECALL_LIBRARY_META"), std::env::var_os("APPDATA")) {
            Some(path) => Self::load_from(path),
            None => Self::default(),
        }
    }

    /// Missing file → empty store. Corrupt file → moved aside as `.bad`, empty store.
    /// Unreadable file (locked, denied) → in-memory store that refuses to overwrite it.
    pub(crate) fn load_from(path: PathBuf) -> Self {
        match std::fs::read(&path) {
            Ok(bytes) => match parse_clips(&bytes) {
                Some(clips) => Self { path: Some(path), clips, ..Self::default() },
                None => {
                    let bad = quarantine_path(&path);
                    let _ = std::fs::remove_file(&bad);
                    if std::fs::rename(&path, &bad).is_err() {
                        let _ = std::fs::copy(&path, &bad);
                    }
                    Self { path: Some(path), ..Self::default() }
                }
            },
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Self { path: Some(path), ..Self::default() },
            Err(error) => Self { write_block: Some(format!("library metadata unreadable ({error}); changes are not saved")), ..Self::default() },
        }
    }

    pub(super) fn meta(&self, file_name: &str) -> Option<&ClipMeta> {
        self.clips.get(file_name)
    }

    pub fn set_favourite(&mut self, file_name: &str, favourite: bool) -> Result<(), String> {
        self.edit(file_name, |meta| meta.favourite = favourite)
    }

    /// `None` or empty clears the custom title.
    pub fn set_title(&mut self, file_name: &str, title: Option<&str>) -> Result<(), String> {
        let title = title.map(str::trim).filter(|t| !t.is_empty()).map(str::to_owned);
        self.edit(file_name, |meta| meta.title = title)
    }

    /// Cached duration keyed by (file name, bytes, mtime).
    pub fn duration(&self, file_name: &str, bytes: u64, mtime: u64) -> Option<f64> {
        let cached = self.clips.get(file_name)?.duration?;
        (cached.bytes == bytes && cached.mtime == mtime).then_some(cached.secs)
    }

    pub fn set_duration(&mut self, file_name: &str, bytes: u64, mtime: u64, secs: f64) {
        if !secs.is_finite() || secs <= 0.0 {
            return;
        }
        let next = Some(CachedDuration { bytes, mtime, secs });
        let meta = self.clips.entry(file_name.to_owned()).or_default();
        if meta.duration != next {
            meta.duration = next;
            self.dirty = true;
        }
    }

    /// Persists pending duration-cache entries (debounced by the caller).
    pub fn flush(&mut self) -> Result<(), String> {
        if !self.dirty {
            return Ok(());
        }
        self.persist()?;
        self.dirty = false;
        Ok(())
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
        if let Err(error) = self.persist() {
            match previous {
                Some(meta) => self.clips.insert(file_name.to_owned(), meta),
                None => self.clips.remove(file_name),
            };
            return Err(error);
        }
        self.dirty = false;
        Ok(())
    }

    /// Whole-file write to a temp sibling, then rename over the original.
    fn persist(&self) -> Result<(), String> {
        if let Some(reason) = &self.write_block {
            return Err(reason.clone());
        }
        let Some(path) = &self.path else { return Ok(()) };
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|error| format!("create library metadata folder: {error}"))?;
        }
        let bytes = serde_json::to_vec(&self.clips).map_err(|error| error.to_string())?;
        let mut temp_name = path.file_name().map(OsString::from).unwrap_or_default();
        temp_name.push(format!(".{}.tmp", std::process::id()));
        let temp = path.with_file_name(temp_name);
        std::fs::write(&temp, bytes).map_err(|error| format!("write library metadata: {error}"))?;
        std::fs::rename(&temp, path).map_err(|error| {
            let _ = std::fs::remove_file(&temp);
            format!("publish library metadata: {error}")
        })
    }
}
