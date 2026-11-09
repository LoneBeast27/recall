//! The library file the storage cap reads favourites from (split from record_storage.rs).
use std::collections::HashSet;
use std::path::{Path, PathBuf};

use super::favourites;

/// `RECALL_LIBRARY_META`, else `%APPDATA%\Recall\library.json`.
pub fn library_path() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("RECALL_LIBRARY_META").filter(|p| !p.is_empty()) {
        return Some(PathBuf::from(p));
    }
    crate::user_dirs::appdata().map(|a| crate::user_dirs::checked(a.join("Recall").join("library.json")))
}

/// Favourites from disk ([`favourites_at`] the [`library_path`]).
pub(super) fn load_favourites() -> Result<HashSet<String>, String> {
    let Some(path) = library_path() else { return Ok(HashSet::new()) };
    favourites_at(&path)
}

/// `library.json` -> `library.json.bad` (where the app moves a corrupt file).
pub fn quarantine_path(path: &Path) -> PathBuf {
    let mut name = path.file_name().map(|n| n.to_os_string()).unwrap_or_else(|| "library.json".into());
    name.push(".bad");
    path.with_file_name(name)
}

/// Favourites in the library file at `path`, plus every favourite still readable
/// in a quarantined `.bad` copy (a former favourite is never recycled). Unknown
/// (Err, the run is cancelled) while the app's damage notice is unconfirmed
/// (`library.json.hold`), when the file is unreadable or corrupt, when it is
/// missing beside a `.bad`, or when a `.bad` is newer than it (the app wrote a
/// fresh one without the old favourites).
pub fn favourites_at(path: &Path) -> Result<HashSet<String>, String> {
    crate::library_damage::check_hold(path)?;
    let bads = crate::library_damage::quarantines(&quarantine_path(path))?;
    let newest = bads.iter().max_by_key(|(_, at)| *at).cloned();
    let mut favs = match std::fs::read(path) {
        Ok(bytes) => {
            if let Some((bad, at)) = newest {
                let lib_at = std::fs::metadata(path).and_then(|m| m.modified()).map_err(|e| format!("{}: {e}", path.display()))?;
                if at > lib_at {
                    return Err(format!("{} holds a quarantined corrupt copy newer than {}", bad.display(), path.display()));
                }
            }
            favourites(&bytes)?
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => match newest {
            None => HashSet::new(),
            Some((bad, _)) => return Err(format!("{} is missing and {} holds a quarantined corrupt copy", path.display(), bad.display())),
        },
        Err(e) => return Err(format!("{}: {e}", path.display())),
    };
    let copies: Vec<PathBuf> = bads.into_iter().map(|(bad, _)| bad).collect();
    favs.extend(crate::library_damage::quarantined_favourites(&copies)?);
    Ok(favs)
}
