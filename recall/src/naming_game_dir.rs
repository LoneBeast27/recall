//! "Sort clips into game folders": which folder a game's new clips go into
//! ([`resolve_game_dir`]), and the check before each write into one ([`check_game_dir`]).
//! A folder counts only when Recall made it (`game_folder_rule`): a folder the user made, or a
//! link, is never adopted.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

use super::{game_folder_name, rule, source_slug, PART_EXT};

/// Paths of new clips stay below this many UTF-16 units (`MAX_PATH` is 260, with room left).
pub(super) const PATH_LIMIT: usize = 250;
/// The longest file-name tail after the slug: a clip being written,
/// `__YYYY-MM-DD__HH-MM-SS-mmm__<32 hex>.mp4.part` (68). A long recording's
/// `__YYYY-MM-DD__HH-MM-SS__rec-NN.mp4.part` (38) is shorter.
pub(super) const LONGEST_TAIL: usize = 2 + 23 + 2 + 32 + ".mp4".len() + PART_EXT.len();

pub(super) fn units(path: &Path) -> usize {
    path.to_string_lossy().encode_utf16().count()
}

/// The folder new clips and long recordings of `source` go into with "Sort clips into game
/// folders" on. For `name` = [`game_folder_name`]:
/// - `<out>\<name>` missing: make it (`create_dir`, only when `<out>` exists) with the marker;
/// - it exists, carries the marker and is a real folder: use it;
/// - otherwise (a folder the user made, a link, a file): never adopt it, try `<name> (Recall)`
///   by the same rule;
/// - neither works, an I/O error, or a clip path that would reach [`PATH_LIMIT`]: `<out>`.
///
/// Cached per clip folder and game for the run, so the disk is checked once per game. A
/// missing `<out>` is not cached (a drive that comes back gets its game folders).
pub fn resolve_game_dir(out_dir: &str, source: &str) -> PathBuf {
    static CACHE: OnceLock<Mutex<HashMap<(String, String), PathBuf>>> = OnceLock::new();
    let name = game_folder_name(source);
    let key = (out_dir.trim_end_matches(['\\', '/']).to_lowercase(), name.to_lowercase());
    let cache = CACHE.get_or_init(Default::default);
    if let Some(dir) = cache.lock().unwrap_or_else(|p| p.into_inner()).get(&key) {
        return dir.clone();
    }
    let out = Path::new(out_dir);
    if !out.is_dir() {
        return out.to_path_buf();
    }
    let dir = resolve_uncached(out, &name, source_slug(source).encode_utf16().count());
    cache.lock().unwrap_or_else(|p| p.into_inner()).insert(key, dir.clone());
    dir
}

fn resolve_uncached(out: &Path, name: &str, slug_units: usize) -> PathBuf {
    for candidate in [name.to_string(), format!("{name} (Recall)")] {
        let dir = out.join(&candidate);
        if units(&dir) + 1 + slug_units + LONGEST_TAIL >= PATH_LIMIT {
            break;
        }
        match std::fs::symlink_metadata(&dir) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return if rule::make(&dir).is_ok() { dir } else { out.to_path_buf() };
            }
            Err(_) => return out.to_path_buf(),
            Ok(_) if rule::is_game_folder(&dir) => return dir,
            Ok(_) => continue,
        }
    }
    out.to_path_buf()
}

/// Why a write into a game folder may not go ahead ([`check_game_dir`]).
#[derive(Debug)]
pub enum GameDirProblem {
    /// The game folder is a junction or symlink: nothing is written through it.
    Link,
    /// It was missing and could not be made again.
    Io(std::io::Error),
}

fn same_dir(a: &Path, b: &Path) -> bool {
    let plain = |p: &Path| p.to_string_lossy().replace('/', "\\").trim_end_matches('\\').to_lowercase();
    plain(a) == plain(b)
}

/// Before a write into `dir`: when `dir` is a game folder of `clip_folder` (one level down),
/// make it again if it went missing (`create_dir` + marker, only while `clip_folder` exists)
/// and refuse a link. Any other `dir` (the clip folder itself, somewhere else) passes.
pub fn check_game_dir(clip_folder: &Path, dir: &Path) -> Result<(), GameDirProblem> {
    if dir.as_os_str().is_empty() || same_dir(dir, clip_folder) || !dir.parent().is_some_and(|p| same_dir(p, clip_folder)) {
        return Ok(());
    }
    let link = || if rule::is_link(dir) { Err(GameDirProblem::Link) } else { Ok(()) };
    match std::fs::symlink_metadata(dir) {
        Ok(_) => link(),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound && clip_folder.is_dir() => match rule::make(dir) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => link(),
            Err(e) => Err(GameDirProblem::Io(e)),
        },
        Err(_) => Ok(()),
    }
}

#[cfg(test)]
#[path = "naming_game_dir_tests.rs"]
mod tests;
