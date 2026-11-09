//! "Sort clips into game folders": which folder a game's new clips go into
//! ([`resolve_game_dir`]), and the check before each write into one ([`check_game_dir`]).
//! A folder counts only when Recall made it or the user said "Use this folder" for it
//! (`game_folder_rule`, `naming_game_choices`). A link, a hidden or system folder, or a linked
//! (Folder Mapper) folder is never adopted.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

use super::game_choices::{choice_for, first_ask, linked_folders, under_link};
use super::{game_folder_name, rule, source_slug, PART_EXT};
use rule::Choice;

/// Paths of new clips stay below this many UTF-16 units (`MAX_PATH` is 260, with room left).
pub(super) const PATH_LIMIT: usize = 250;
/// The longest file-name tail after the slug: a clip being written,
/// `__YYYY-MM-DD__HH-MM-SS-mmm__<32 hex>.mp4.part` (68). A long recording's
/// `__YYYY-MM-DD__HH-MM-SS__rec-NN.mp4.part` (38) is shorter.
pub(super) const LONGEST_TAIL: usize = 2 + 23 + 2 + 32 + ".mp4".len() + PART_EXT.len();

pub(super) fn units(path: &Path) -> usize {
    path.to_string_lossy().encode_utf16().count()
}

type Cache = Mutex<HashMap<(String, String), PathBuf>>;

fn cache() -> &'static Cache {
    static CACHE: OnceLock<Cache> = OnceLock::new();
    CACHE.get_or_init(Default::default)
}

fn cache_key(out_dir: &str, name: &str) -> (String, String) {
    (out_dir.trim_end_matches(['\\', '/']).to_lowercase(), name.to_lowercase())
}

/// The folder new clips and long recordings of `source` go into with "Sort clips into game
/// folders" on. For `name` = [`game_folder_name`]:
/// - `<out>\<name>` missing: make it (`create_dir`, only when `<out>` exists) with the marker;
/// - it is a game folder (a readable marker, a real folder): use it;
/// - it is the user's real folder (no marker, not a link, not hidden or system, not linked in
///   the app): the answer to "Use your existing <Game> folder?" decides. `merge`: adopt it
///   (`rule::adopt`) and use it. `separate`: `<name> (Recall)`. No answer yet: `<out>` itself
///   (not cached, so the next save looks again) and one `game_folder_exists` per game per run;
/// - otherwise (a link, hidden, system, a file): `<name> (Recall)` by the same rule;
/// - neither works, an I/O error, or a clip path that would reach [`PATH_LIMIT`]: `<out>`.
///
/// Cached per clip folder and game for the run, so the disk is checked once per game. A
/// missing `<out>` is not cached (a drive that comes back gets its game folders). Disk work:
/// call it from a writer thread (the saver, the long-recording thread), never a hotkey.
pub fn resolve_game_dir(out_dir: &str, source: &str) -> PathBuf {
    let ask = |folder: &Path| {
        crate::protocol::emit(&crate::protocol::Outbound::GameFolderExists { game: source.to_string(), folder: folder.to_string_lossy().into_owned() });
    };
    resolve_with(out_dir, source, &Inputs { linked: &linked_folders(), choose: &choice_for, ask: &ask })
}

/// What [`resolve_game_dir`] reads besides the disk (tests pass their own).
pub(super) struct Inputs<'a> {
    /// The app's linked (Folder Mapper) folders.
    pub linked: &'a [String],
    /// The answer for a folder name.
    pub choose: &'a dyn Fn(&str) -> Option<Choice>,
    /// Sends the question for a folder.
    pub ask: &'a dyn Fn(&Path),
}

pub(super) fn resolve_with(out_dir: &str, source: &str, inputs: &Inputs) -> PathBuf {
    let name = game_folder_name(source);
    let key = cache_key(out_dir, &name);
    if let Some(dir) = cache().lock().unwrap_or_else(|p| p.into_inner()).get(&key) {
        return dir.clone();
    }
    let out = Path::new(out_dir);
    if !out.is_dir() {
        return out.to_path_buf();
    }
    let slug_units = source_slug(source).encode_utf16().count();
    let dir = match resolve_uncached(out, &name, slug_units, inputs) {
        Resolved::Dir(dir) => dir,
        Resolved::Ask(folder) => {
            if first_ask(out_dir, &name) {
                (inputs.ask)(&folder);
            }
            return out.to_path_buf();
        }
    };
    cache().lock().unwrap_or_else(|p| p.into_inner()).insert(key, dir.clone());
    dir
}

/// Drop the cached folder of `source` in `out_dir`, so the next [`resolve_game_dir`] checks the
/// disk again (the cached one stopped being Recall's: [`GameDirProblem::NotOurs`]).
pub fn forget_game_dir(out_dir: &str, source: &str) {
    cache().lock().unwrap_or_else(|p| p.into_inner()).remove(&cache_key(out_dir, &game_folder_name(source)));
}

enum Resolved {
    Dir(PathBuf),
    /// The user's folder with the game's name, no answer yet: ask, save in `<out>` meanwhile.
    Ask(PathBuf),
}

fn resolve_uncached(out: &Path, name: &str, slug_units: usize, inputs: &Inputs) -> Resolved {
    let linked = inputs.linked;
    for (i, candidate) in [name.to_string(), format!("{name} (Recall)")].into_iter().enumerate() {
        let dir = out.join(&candidate);
        if units(&dir) + 1 + slug_units + LONGEST_TAIL >= PATH_LIMIT {
            break;
        }
        match std::fs::symlink_metadata(&dir) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Resolved::Dir(if rule::make(&dir).is_ok() { dir } else { out.to_path_buf() });
            }
            Err(_) => return Resolved::Dir(out.to_path_buf()),
            Ok(_) if rule::is_game_folder(&dir) && !under_link(&dir, out, linked) => return Resolved::Dir(dir),
            // Only the game's own name can be adopted; "(Recall)" is Recall's name.
            Ok(_) if i == 0 && rule::is_plain_dir(&dir) && !rule::has_marker(&dir) && !under_link(&dir, out, linked) => match (inputs.choose)(name) {
                Some(Choice::Merge) => {
                    let adopted = rule::adopt(&dir).is_ok() && rule::is_game_folder(&dir);
                    return Resolved::Dir(if adopted { dir } else { out.to_path_buf() });
                }
                Some(Choice::Separate) => continue,
                None => return Resolved::Ask(dir),
            },
            Ok(_) => continue,
        }
    }
    Resolved::Dir(out.to_path_buf())
}

/// Why a write into a game folder may not go ahead ([`check_game_dir`]).
#[derive(Debug)]
pub enum GameDirProblem {
    /// The game folder is a junction or symlink: nothing is written through it.
    Link,
    /// It exists but is not Recall's game folder now (no marker, hidden or system: the user made
    /// their own folder in its place mid-run): nothing is written into it.
    NotOurs,
    /// It was missing and could not be made again.
    Io(std::io::Error),
}

fn same_dir(a: &Path, b: &Path) -> bool {
    let plain = |p: &Path| p.to_string_lossy().replace('/', "\\").trim_end_matches('\\').to_lowercase();
    plain(a) == plain(b)
}

/// Before a write into `dir`: when `dir` is a game folder of `clip_folder` (one level down),
/// make it again if it went missing (`create_dir` + marker, only while `clip_folder` exists),
/// refuse a link, and refuse a folder that is not [`rule::is_game_folder`]. Any other `dir`
/// (the clip folder itself, somewhere else) passes.
pub fn check_game_dir(clip_folder: &Path, dir: &Path) -> Result<(), GameDirProblem> {
    if dir.as_os_str().is_empty() || same_dir(dir, clip_folder) || !dir.parent().is_some_and(|p| same_dir(p, clip_folder)) {
        return Ok(());
    }
    let ours = || {
        if rule::is_link(dir) {
            Err(GameDirProblem::Link)
        } else if !rule::is_game_folder(dir) {
            Err(GameDirProblem::NotOurs)
        } else {
            Ok(())
        }
    };
    match std::fs::symlink_metadata(dir) {
        Ok(_) => ours(),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound && clip_folder.is_dir() => match rule::make(dir) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => ours(),
            Err(e) => Err(GameDirProblem::Io(e)),
        },
        Err(_) => Ok(()),
    }
}

/// The folder a clip or long recording of `source` is written into now: its game folder
/// (resolved and checked here, on the writer's thread), or `clip_folder` itself when that
/// folder is not usable (a link, not Recall's any more, can't be made). On a fallback the cache
/// entry is dropped and `log` gets one line; the write never fails for it.
pub fn writable_game_dir(clip_folder: &str, source: &str, log: &dyn Fn(&str)) -> PathBuf {
    let out = Path::new(clip_folder);
    let dir = resolve_game_dir(clip_folder, source);
    let why = match check_game_dir(out, &dir) {
        Ok(()) => return dir,
        Err(GameDirProblem::Link) => "is a link".to_string(),
        Err(GameDirProblem::NotOurs) => "is not Recall's game folder now".to_string(),
        Err(GameDirProblem::Io(e)) => format!("could not be made ({e})"),
    };
    forget_game_dir(clip_folder, source);
    log(&format!("game folder {} {why}; saving in {} instead", dir.display(), out.display()));
    out.to_path_buf()
}

#[cfg(test)]
#[path = "naming_game_dir_tests.rs"]
mod tests;
