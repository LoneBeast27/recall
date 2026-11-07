//! Hand edits of config.toml while Recall runs. The controller loads the file once and
//! patches its in-memory copy, so the next Settings click (or Alt+P / Alt+O) used to write
//! that copy over an edit made in the meantime. Before a patch, a file whose modified time
//! or size moved since the controller last read or wrote it is read again and becomes the
//! base the patch merges into.
//!
//! The text read here is the text loaded (`config::load_bytes`: the same per-key salvage and
//! repair as a start-up load), so an editor that truncates and rewrites is never caught by a
//! second read. A wrongly typed value costs only its key. An edit that is not TOML at all is
//! copied to a dated `config.toml.bad-*` before the patch's save replaces it, and the app is
//! told. An empty file, or one written in the last [`SETTLE`], is never the base.

use std::path::Path;
use std::time::{Duration, SystemTime};

use crate::config::Config;
use crate::durable::{self, Keep};
use crate::log;
use crate::record_config::ConfigStore;

/// A file modified this recently may still be being written: no patch merges into it.
pub const SETTLE: Duration = Duration::from_secs(2);

/// What the file looked like when the controller last read or wrote it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FileStamp {
    modified: Option<SystemTime>,
    len: u64,
}

impl FileStamp {
    /// `None` when the file is missing or unreadable.
    pub fn of(path: &Path) -> Option<FileStamp> {
        let meta = std::fs::metadata(path).ok()?;
        Some(FileStamp { modified: meta.modified().ok(), len: meta.len() })
    }

    /// Modified less than [`SETTLE`] before `now` (a time ahead of `now` counts as settled,
    /// so a clock change can't block patches for good).
    fn settling(&self, now: SystemTime) -> bool {
        self.modified.is_some_and(|m| now.duration_since(m).is_ok_and(|age| age < SETTLE))
    }
}

/// The base a patch merges into.
#[derive(Debug)]
pub enum Base {
    /// The config in memory (the file is unchanged, gone, unreadable or empty).
    Memory,
    /// The file changed on disk: this is its config (salvaged and repaired like a load).
    Edited(Config),
    /// The file changed but is not TOML: the config in memory is the base, and this line
    /// (naming the backup) goes to the app once the patch is saved.
    Broken(String),
    /// The file changed in the last [`SETTLE`]: the patch is refused, so a save can't race
    /// the editor still writing it.
    Busy,
}

/// The base for a patch arriving at `now`, given what the file looked like when last seen.
/// `seen` moves to the file's new stamp once its text was read and used (or backed up).
pub fn base_for_patch(store: &ConfigStore, seen: &mut Option<FileStamp>, now: SystemTime) -> Base {
    let path = store.path();
    let Some(stamp) = FileStamp::of(path) else { return Base::Memory };
    if Some(stamp) == *seen {
        return Base::Memory;
    }
    if stamp.settling(now) {
        log!("config: config.toml changed on disk moments ago; not saving over it yet");
        return Base::Busy;
    }
    let Ok(bytes) = std::fs::read(path) else { return Base::Memory };
    let text = std::str::from_utf8(&bytes).ok();
    if text.is_some_and(|t| t.trim().is_empty()) {
        log!("config: config.toml is empty on disk; keeping the settings in use");
        return Base::Memory;
    }
    if !text.is_some_and(|t| toml::from_str::<toml::Table>(t).is_ok()) {
        *seen = Some(stamp);
        let kept = durable::quarantine(path, now, Keep::Copy);
        let name = kept.as_deref().and_then(Path::file_name).map(|n| n.to_string_lossy().into_owned());
        log!("config: config.toml changed on disk but is not valid TOML; kept as {name:?}, keeping the settings in use");
        return Base::Broken(match name {
            Some(name) => format!("config.toml had an error (kept as {name}); your other settings were saved"),
            None => "config.toml had an error and could not be backed up; your other settings were saved".into(),
        });
    }
    *seen = Some(stamp);
    log!("config: config.toml changed on disk; applying the edit before this change");
    Base::Edited(crate::config::load_bytes(path, &bytes, now))
}

#[cfg(test)]
#[path = "config_watch_tests.rs"]
mod tests;
