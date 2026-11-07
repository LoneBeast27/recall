//! The storage cap's view of a damaged library.json (deep-audit round 2, R2-1, R2-2).
//!
//! - While the app shows its "Library data was damaged" notice it keeps
//!   `library.json.hold` beside the file; the cap does not run until the user
//!   presses "Keep going" there (the app then removes it).
//! - Favourites still readable in any `library.json.bad*` copy (read leniently, a
//!   cut-off file keeps the entries before the cut) always count, so a former
//!   favourite is never recycled, whatever the app wrote since.
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use serde_json::Value;

/// `library.json.hold`: set by the app while its damage notice is unconfirmed.
pub fn hold_path(path: &Path) -> PathBuf {
    let mut name = path.file_name().map(|n| n.to_os_string()).unwrap_or_else(|| "library.json".into());
    name.push(".hold");
    path.with_file_name(name)
}

/// Err while the app's damage notice is unconfirmed.
pub fn check_hold(path: &Path) -> Result<(), String> {
    let hold = hold_path(path);
    match std::fs::metadata(&hold) {
        Ok(_) => Err(format!("{} is set: the app found the library data damaged and the user has not confirmed it yet", hold.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(format!("{}: {e}", hold.display())),
    }
}

/// Lowercase names marked favourite in what still parses of `bytes`.
pub fn lenient_favourites(bytes: &[u8]) -> HashSet<String> {
    crate::lenient_json::partial_object(bytes)
        .into_iter()
        .filter(|(_, v)| v.get("favourite").and_then(Value::as_bool) == Some(true))
        .map(|(name, _)| name.to_lowercase())
        .collect()
}

/// Favourites in every `bads` copy; Err when one can't be read (it may hold some).
pub fn quarantined_favourites(bads: &[PathBuf]) -> Result<HashSet<String>, String> {
    let mut out = HashSet::new();
    for bad in bads {
        let bytes = std::fs::read(bad).map_err(|e| format!("{}: {e}", bad.display()))?;
        out.extend(lenient_favourites(&bytes));
    }
    Ok(out)
}

/// Every copy whose name starts with `bad` (`library.json.bad`): the app keeps them as
/// `library.json.bad-YYYY-MM-DD[-N]`, stamped when it found the damage.
pub fn quarantines(bad: &Path) -> Result<Vec<(PathBuf, SystemTime)>, String> {
    let prefix = bad.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let Some(dir) = bad.parent() else { return Ok(vec![]) };
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(vec![]),
        Err(e) => return Err(format!("{}: {e}", dir.display())),
    };
    let mut out = Vec::new();
    for entry in entries.flatten() {
        if !entry.file_name().to_string_lossy().starts_with(&prefix) {
            continue;
        }
        let at = entry.metadata().and_then(|m| m.modified()).map_err(|e| format!("{}: {e}", entry.path().display()))?;
        out.push((entry.path(), at));
    }
    Ok(out)
}
