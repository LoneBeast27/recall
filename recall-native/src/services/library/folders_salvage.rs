//! A damaged folders.json (review RI-1). The app reads the file again for every listing, the
//! start-up kept sweep and each Settings save, so the folders still readable in it must be on
//! disk, not only in the UI's copy: moving the damaged file aside left every later load with
//! defaults, and the next folder save wrote the imported folders and "Folders Recall manages"
//! away for good.
use std::path::Path;
use std::time::SystemTime;

use super::Folders;
use crate::services::durable::{quarantine, Keep};

impl Folders {
    /// `bytes`, read from `path`, did not parse. When some folders are still readable, the
    /// damaged file is copied to a dated `.bad` and those folders are written back durably;
    /// when none are, it is moved aside. With no backup, or a failed write-back, the damaged
    /// file stays where it is, so the next load reads the same folders back from it.
    pub(super) fn recover(path: &Path, bytes: &[u8], now: SystemTime) -> Self {
        let mut folders = Self::salvage(bytes);
        let found = folders.recordings.is_some() || !folders.imported.is_empty() || !folders.former_recordings.is_empty();
        let backup = quarantine(path, now, if found { Keep::Copy } else { Keep::Move });
        let mut notice = notice(&folders, backup.as_deref());
        let written = if found && backup.is_some() { folders.save_to(path) } else { Ok(()) };
        if let Err(error) = written {
            eprintln!("folders.json: the folders read back were not written back: {error}");
            notice.push_str(" They could not be saved yet.");
        }
        folders.damaged = Some(notice);
        folders
    }

    /// The folders still readable in a damaged file.
    fn salvage(bytes: &[u8]) -> Self {
        let found = crate::services::lenient_json::partial_object(bytes);
        let text = |v: &serde_json::Value| v.as_str().map(str::to_owned);
        let recordings = found.get("recordings").and_then(text);
        let list = |key: &str| -> Vec<String> { found.get(key).and_then(|v| v.as_array()).map(|list| list.iter().filter_map(text).collect()).unwrap_or_default() };
        // L15: the folders Recall manages are read back too (their clips keep Delete-to-bin).
        Self { recordings, imported: list("imported"), former_recordings: list("former_recordings"), ..Self::default() }
    }
}

/// The notice that says what was read back, and where the damaged file is kept.
fn notice(folders: &Folders, backup: Option<&Path>) -> String {
    let count = folders.imported.len();
    let noun = if count == 1 { "folder" } else { "folders" };
    let kept = backup.and_then(Path::file_name).map(|n| format!(" The damaged file is kept as {}.", n.to_string_lossy())).unwrap_or_default();
    format!("{count} imported {noun} read back; add any missing ones again in Settings.{kept}")
}
