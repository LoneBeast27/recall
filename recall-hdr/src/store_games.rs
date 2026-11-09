//! Xbox / Microsoft Store packaged games (official proof, the maintainer 2026-11-07
//! 07:46 UTC): an exe inside a package whose install folder holds
//! `MicrosoftGame.config`, the GDK game manifest. Gaming Services writes it
//! into every game it installs, in `<drive>:\XboxGames\<game>\Content` (seen
//! as `MicrosoftGame.Config` on this PC) or `Program Files\WindowsApps\<package>`;
//! a plain Store app has none. Read at run time from the exe's own folders,
//! nothing hard-coded.
//!
//! The exe's folder and up to [`MAX_LEVELS`] - 1 parents are looked in
//! (`Content\bin\x64\Game.exe` finds `Content`); never a drive or share root,
//! never a network path (a stat there can stall the pump). Each folder's
//! answer is cached for the run, so a foreground change costs at most a few
//! file stats once per folder.
//!
//! Like a launcher folder it is not evidence against the private list, and
//! it proves a fullscreen window only (`WatchEvent::store_game`): the
//! package of a launcher (the Minecraft Launcher ships a config too) is not
//! a windowed game.

use std::collections::HashMap;
use std::sync::Mutex;

/// The manifest's file name (any case on disk).
pub(crate) const GAME_CONFIG: &str = "MicrosoftGame.config";
/// Folders looked in: the exe's own and its parents.
pub(crate) const MAX_LEVELS: usize = 6;
/// Cached folders before the cache starts over.
const CACHE_MAX: usize = 1024;

/// The folders of `exe_path` to look in, nearest first. Empty for a network
/// or relative path.
pub(crate) fn package_dirs(exe_path: &str) -> Vec<String> {
    let path = exe_path.trim().replace('/', "\\");
    let path = path.strip_prefix("\\\\?\\").unwrap_or(&path).to_string();
    if path.starts_with("\\\\") || path.as_bytes().get(1) != Some(&b':') {
        return Vec::new();
    }
    let mut parts: Vec<&str> = path.split('\\').filter(|s| !s.is_empty()).collect();
    parts.pop(); // the file name
    let mut out = Vec::new();
    while parts.len() > 1 && out.len() < MAX_LEVELS {
        out.push(parts.join("\\"));
        parts.pop();
    }
    out
}

/// `exe_path` lies in a game package: one of its [`package_dirs`] holds the
/// manifest (`has_config(dir)`).
pub(crate) fn in_game_package_with(exe_path: &str, mut has_config: impl FnMut(&str) -> bool) -> bool {
    package_dirs(exe_path).iter().any(|d| has_config(d))
}

static SEEN: Mutex<Option<HashMap<String, bool>>> = Mutex::new(None);

/// [`in_game_package_with`] on the real disk, cached per folder.
pub(crate) fn is_store_game_path(exe_path: &str) -> bool {
    in_game_package_with(exe_path, |dir| {
        let key = dir.to_ascii_lowercase();
        let mut seen = SEEN.lock().unwrap_or_else(|e| e.into_inner());
        let cache = seen.get_or_insert_with(HashMap::new);
        if let Some(found) = cache.get(&key) {
            return *found;
        }
        let found = std::path::Path::new(dir).join(GAME_CONFIG).is_file();
        if cache.len() >= CACHE_MAX {
            cache.clear();
        }
        cache.insert(key, found);
        found
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_exe_folder_and_its_parents_are_looked_in_never_a_root() {
        assert_eq!(
            package_dirs(r"G:\XboxGames\Fixture\Content\bin\Game.exe"),
            vec![r"G:\XboxGames\Fixture\Content\bin", r"G:\XboxGames\Fixture\Content", r"G:\XboxGames\Fixture", r"G:\XboxGames"]
        );
        assert!(package_dirs(r"G:\Game.exe").is_empty(), "a drive root is never looked in");
        assert!(package_dirs(r"\\server\share\Game\Game.exe").is_empty(), "network paths are skipped");
        assert!(package_dirs("Game.exe").is_empty());
        assert_eq!(package_dirs(r"\\?\G:\A\B.exe"), vec![r"G:\A"]);
        assert_eq!(package_dirs(r"G:\1\2\3\4\5\6\7\8\x.exe").len(), MAX_LEVELS);
    }

    #[test]
    fn a_config_in_any_looked_in_folder_is_a_package() {
        let has = |dir: &str| dir.eq_ignore_ascii_case(r"G:\XboxGames\Fixture\Content");
        assert!(in_game_package_with(r"G:\XboxGames\Fixture\Content\Game.exe", has));
        assert!(in_game_package_with(r"G:\XboxGames\Fixture\Content\bin\x64\Game.exe", has));
        assert!(!in_game_package_with(r"G:\XboxGames\Other\Content\Game.exe", has));
    }
}
