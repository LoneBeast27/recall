//! Which game library an exe lives in (split out of `watcher`, file size):
//! a Steam library (`steamapps\common`) or another launcher's
//! (`launcher_libraries`). The two are not the same evidence (R7-detect-1):
//!
//! - both make a windowed exe a candidate with no render check
//!   (`WatchEvent::steam_game` / `launcher_game`);
//! - only a Steam library beats the private list (`record_target::game_evidence`).
//!   Steam owns `steamapps\common`; a launcher folder is whatever the user or
//!   the registry says, so a chat app there stays private unless opted in.
//!
//! A prerequisite installer shipped inside a game folder (`_CommonRedist`,
//! `__redist`, `__Installer`, `Engine\Extras\Redist`, `DirectX`, `vcredist`)
//! is no game in either (R7-detect-5).

/// Where an exe lives, as far as game evidence goes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Library {
    /// Neither (or a prerequisite installer inside one).
    None,
    /// A Steam library (`steamapps\common`).
    Steam,
    /// Another launcher's game folder (Epic, GOG, Ubisoft, EA, Battle.net, itch).
    Launcher,
}

/// Folder names (whole path segments, any case) that hold a game's
/// prerequisite installers rather than the game.
const PREREQ_DIRS: &[&str] = &["_commonredist", "__redist", "__installer", "redist", "directx", "vcredist"];

/// A folder of `path` (not the file name) is a prerequisite folder.
pub(crate) fn is_prereq_path(path: &str) -> bool {
    let path = path.replace('/', "\\");
    let mut dirs = path.split('\\').collect::<Vec<_>>();
    dirs.pop();
    dirs.iter().any(|d| PREREQ_DIRS.iter().any(|p| p.eq_ignore_ascii_case(d.trim())))
}

/// Whether an executable path is a game in a Steam library. Steamworks Shared
/// (the redistributable installers Steam runs before a first launch) and a
/// game's own prerequisite folders are not.
pub(crate) fn is_steam_library_path(path: &str) -> bool {
    let lower = path.to_ascii_lowercase().replace('/', "\\");
    lower.contains("\\steamapps\\common\\") && !lower.contains("\\steamapps\\common\\steamworks shared\\") && !is_prereq_path(&lower)
}

/// The library `path` lives in: Steam, else a known launcher's game folder
/// (the cached folders of `launcher_libraries`), minus prerequisite folders.
pub(crate) fn game_library(path: &str) -> Library {
    library_with(path, crate::launcher_libraries::is_launcher_library_path)
}

/// [`game_library`] against given launcher folders (tests).
#[cfg(test)]
pub(crate) fn game_library_in(path: &str, roots: &[String]) -> Library {
    library_with(path, |p| crate::launcher_libraries::is_under(p, roots))
}

/// Any library at all (tests).
#[cfg(test)]
pub(crate) fn is_game_library_path_in(path: &str, roots: &[String]) -> bool {
    game_library_in(path, roots) != Library::None
}

fn library_with(path: &str, in_launcher: impl Fn(&str) -> bool) -> Library {
    if is_steam_library_path(path) {
        Library::Steam
    } else if !is_prereq_path(path) && in_launcher(path) {
        Library::Launcher
    } else {
        Library::None
    }
}
