//! Clips the storage cap must leave alone besides favourites and edits: a clip played in the
//! last [`PLAYED_SECS`] (`library.json` `played`, Unix seconds, stamped when the app opens it)
//! and the clip open in the app's editor or player (`open-clips.json` beside `library.json`,
//! written by the app: `services/library/open_clips.rs`). Both are read again before every
//! Recycle Bin call ([`kept_now`]), so a clip starred, played or opened during a long run
//! is skipped too.
//!
//! Keys may be file names or full paths (the app's newer key shape); both match by file name,
//! so a clip of the same name in another folder is kept as well (keeping more is the safe side).

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::Value;

/// A clip played this recently (seconds) is never removed: it may be on screen.
pub const PLAYED_SECS: u64 = 600;

/// Lowercase file name of a `library.json` / `open-clips.json` key or path.
pub fn file_key(key: &str) -> String {
    Path::new(key).file_name().map_or_else(|| key.to_lowercase(), |n| n.to_string_lossy().to_lowercase())
}

/// Clips whose `played` stamp is within [`PLAYED_SECS`] of `now_unix` (a stamp in the future,
/// after a clock change, counts as recent). An unreadable file gives none: favourites
/// (read from the same file) already cancel the run then.
pub fn played_recently(json_bytes: &[u8], now_unix: u64) -> HashSet<String> {
    let Ok(Value::Object(map)) = serde_json::from_slice::<Value>(json_bytes) else { return HashSet::new() };
    map.into_iter()
        .filter(|(_, v)| v.get("played").and_then(Value::as_u64).is_some_and(|at| at.saturating_add(PLAYED_SECS) > now_unix))
        .map(|(key, _)| file_key(&key))
        .collect()
}

/// `<folder of library.json>\open-clips.json`.
pub fn open_clips_path(library: &Path) -> PathBuf {
    library.with_file_name("open-clips.json")
}

/// Clips the app has open (`{"paths": [...]}`). Missing = none; unreadable or corrupt =
/// unknown, so the run stops (the app rewrites the file at its next start).
pub fn open_clips(path: &Path) -> Result<HashSet<String>, String> {
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(HashSet::new()),
        Err(e) => return Err(format!("{}: {e}", path.display())),
    };
    let value: Value = serde_json::from_slice(&bytes).map_err(|e| format!("{}: {e}", path.display()))?;
    let paths = value.get("paths").and_then(Value::as_array).ok_or_else(|| format!("{} has no paths list", path.display()))?;
    Ok(paths.iter().filter_map(Value::as_str).map(file_key).collect())
}

/// Played-recently and open clips for the library file at `library`.
pub fn recent_and_open(library: &Path, now_unix: u64) -> Result<HashSet<String>, String> {
    let mut keep = std::fs::read(library).map(|bytes| played_recently(&bytes, now_unix)).unwrap_or_default();
    keep.extend(open_clips(&open_clips_path(library))?);
    Ok(keep)
}

/// Everything the cap must keep now: favourites (`favourites_at`), recently played and open
/// clips. Err = unknown, delete nothing. The production `KeepFn`.
pub fn kept_now() -> Result<HashSet<String>, String> {
    let mut keep = super::load_favourites()?;
    if let Some(library) = super::library_path() {
        let now = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
        keep.extend(recent_and_open(&library, now)?);
    }
    Ok(keep)
}
