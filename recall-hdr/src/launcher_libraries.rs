//! Game libraries of the other PC launchers: an exe under one of these
//! folders is game evidence exactly like a Steam library path
//! (`watcher::is_steam_library_path`, R6 merge gate). The folders are read at
//! run time from what each launcher writes, never from hard-coded drives:
//!
//! - Epic: `%ProgramData%\Epic\EpicGamesLauncher\Data\Manifests\*.item`
//!   (JSON), `InstallLocation`; an entry whose `AppCategories` does not say
//!   `games` (the Unreal Engine, plugins) is skipped;
//! - GOG: `HKLM\SOFTWARE\WOW6432Node\GOG.com\Games\*` value `path`;
//! - Ubisoft Connect: `HKLM\SOFTWARE\WOW6432Node\Ubisoft\Launcher\Installs\*`
//!   value `InstallDir`;
//! - EA app (and Origin): `HKLM\SOFTWARE\WOW6432Node\Electronic Arts\*` and
//!   `...\EA Games\*` value `Install Dir` (the client's own keys are skipped);
//! - Battle.net: the uninstall entries (`...\CurrentVersion\Uninstall\*`)
//!   whose `Publisher` is Blizzard Entertainment, `InstallLocation`, minus
//!   the Battle.net client itself;
//! - itch: `%APPDATA%\itch\apps` (its default install location) and the
//!   install locations in `%APPDATA%\itch\preferences.json`. Locations kept
//!   only in itch's `butler.db` (SQLite) are not read.
//!
//! A missing launcher reads as nothing, a malformed manifest or value is
//! skipped. A folder that would cover far more than one game (a drive root,
//! Program Files, the user profile) is dropped. Lookups read a cached list
//! (`is_launcher_library_path`); a background thread rescans every
//! [`REFRESH_EVERY`] (`launcher_libraries_win`), so the pump never touches
//! the registry or the disk for it.

use std::sync::{Once, RwLock};
use std::time::Duration;

/// How often the cached folders are read again (a game installed while
/// Recall runs counts from the next scan).
pub(crate) const REFRESH_EVERY: Duration = Duration::from_secs(300);

/// A registry hive the readers can open.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum Hive {
    LocalMachine,
}

/// What the scan reads; the live one is `launcher_libraries_win::WinSources`,
/// tests inject fixtures.
pub(crate) trait LibrarySources {
    /// An environment variable (`ProgramData`, `APPDATA`, `ProgramFiles`, ...).
    fn env(&self, name: &str) -> Option<String>;
    /// The file names in `dir` (empty when it does not exist).
    fn list_dir(&self, dir: &str) -> Vec<String>;
    /// A text file's contents.
    fn read_file(&self, path: &str) -> Option<String>;
    /// Each subkey of `path` with its string `values` (in order, `None` when
    /// absent). Empty when the key does not exist.
    fn reg_subkeys(&self, hive: Hive, path: &str, values: &[&str]) -> Vec<(String, Vec<Option<String>>)>;
}

/// A folder as matched: lower case, `\` separators, no `\\?\` prefix, one
/// trailing `\` (so `games\foo` does not match `games\foobar`).
pub(crate) fn normalize(path: &str) -> String {
    let p = path.trim().replace('/', "\\");
    let p = p.strip_prefix("\\\\?\\").unwrap_or(&p);
    let mut p = p.trim_end_matches('\\').to_ascii_lowercase();
    p.push('\\');
    p
}

/// `exe_path` lies under one of `roots` (as [`normalize`] made them).
pub(crate) fn is_under(exe_path: &str, roots: &[String]) -> bool {
    if roots.is_empty() {
        return false;
    }
    let p = normalize(exe_path);
    roots.iter().any(|r| p.len() > r.len() && p.starts_with(r.as_str()))
}

/// Every launcher's game folders, normalized, de-duplicated, broad folders
/// dropped.
pub(crate) fn collect_roots(src: &dyn LibrarySources) -> Vec<String> {
    let mut found = epic_roots(src);
    found.extend(gog_roots(src));
    found.extend(ubisoft_roots(src));
    found.extend(ea_roots(src));
    found.extend(battlenet_roots(src));
    found.extend(itch_roots(src));
    let broad = broad_folders(src);
    let mut roots: Vec<String> = found.iter().map(|p| normalize(p)).filter(|r| !too_broad(r, &broad)).collect();
    roots.sort();
    roots.dedup();
    roots
}

/// Folders that hold far more than one game.
fn broad_folders(src: &dyn LibrarySources) -> Vec<String> {
    let vars = ["ProgramFiles", "ProgramFiles(x86)", "ProgramW6432", "ProgramData", "SystemRoot", "USERPROFILE", "APPDATA", "LOCALAPPDATA", "PUBLIC"];
    vars.iter().filter_map(|v| src.env(v)).filter(|p| !p.trim().is_empty()).map(|p| normalize(&p)).collect()
}

/// A drive root (`c:\`), a share root, a bare relative name or a broad folder.
fn too_broad(root: &str, broad: &[String]) -> bool {
    let parts = root.trim_end_matches('\\').split('\\').filter(|s| !s.is_empty()).count();
    let drive = root.as_bytes().get(1) == Some(&b':');
    let unc = root.starts_with("\\\\");
    let shallow = if unc { parts <= 2 } else { !drive || parts <= 1 };
    shallow || broad.iter().any(|b| b == root) || root.ends_with(":\\users\\")
}

fn epic_roots(src: &dyn LibrarySources) -> Vec<String> {
    let Some(pd) = src.env("ProgramData") else { return Vec::new() };
    let dir = format!("{}\\Epic\\EpicGamesLauncher\\Data\\Manifests", pd.trim_end_matches('\\'));
    let items = src.list_dir(&dir).into_iter().filter(|n| n.to_ascii_lowercase().ends_with(".item"));
    items.filter_map(|n| src.read_file(&format!("{dir}\\{n}"))).filter_map(|text| epic_install(&text)).collect()
}

/// One Epic manifest's install folder, if it is a game.
pub(crate) fn epic_install(text: &str) -> Option<String> {
    let v: serde_json::Value = serde_json::from_str(text.trim_start_matches('\u{feff}')).ok()?;
    if let Some(cats) = v.get("AppCategories").and_then(|c| c.as_array()) {
        if !cats.iter().any(|c| c.as_str().is_some_and(|s| s.eq_ignore_ascii_case("games"))) {
            return None;
        }
    }
    v.get("InstallLocation")?.as_str().map(str::to_string).filter(|s| !s.trim().is_empty())
}

/// Subkeys' first value, non-empty.
fn reg_values(src: &dyn LibrarySources, path: &str, value: &str) -> Vec<(String, String)> {
    let keys = src.reg_subkeys(Hive::LocalMachine, path, &[value]);
    keys.into_iter().filter_map(|(k, vals)| vals.into_iter().next().flatten().filter(|v| !v.trim().is_empty()).map(|v| (k, v))).collect()
}

fn gog_roots(src: &dyn LibrarySources) -> Vec<String> {
    reg_values(src, "SOFTWARE\\WOW6432Node\\GOG.com\\Games", "path").into_iter().map(|(_, v)| v).collect()
}

fn ubisoft_roots(src: &dyn LibrarySources) -> Vec<String> {
    reg_values(src, "SOFTWARE\\WOW6432Node\\Ubisoft\\Launcher\\Installs", "InstallDir").into_iter().map(|(_, v)| v).collect()
}

/// The EA app's and Origin's own keys under `Electronic Arts`.
const EA_CLIENT_KEYS: &[&str] = &["ea desktop", "ea core", "origin", "ea app"];

fn ea_roots(src: &dyn LibrarySources) -> Vec<String> {
    let mut out = Vec::new();
    for base in ["SOFTWARE\\WOW6432Node\\Electronic Arts", "SOFTWARE\\WOW6432Node\\EA Games"] {
        let games = reg_values(src, base, "Install Dir").into_iter();
        out.extend(games.filter(|(k, _)| !EA_CLIENT_KEYS.contains(&k.to_ascii_lowercase().as_str())).map(|(_, v)| v));
    }
    out
}

fn battlenet_roots(src: &dyn LibrarySources) -> Vec<String> {
    let path = "SOFTWARE\\WOW6432Node\\Microsoft\\Windows\\CurrentVersion\\Uninstall";
    let keys = src.reg_subkeys(Hive::LocalMachine, path, &["Publisher", "InstallLocation", "DisplayName"]);
    keys.into_iter()
        .filter_map(|(key, vals)| {
            let [publisher, location, name] = <[Option<String>; 3]>::try_from(vals).ok()?;
            let blizzard = publisher.is_some_and(|p| p.trim().eq_ignore_ascii_case("Blizzard Entertainment"));
            let client = key.eq_ignore_ascii_case("Battle.net") || name.is_some_and(|n| n.trim().eq_ignore_ascii_case("Battle.net"));
            location.filter(|l| blizzard && !client && !l.trim().is_empty())
        })
        .collect()
}

fn itch_roots(src: &dyn LibrarySources) -> Vec<String> {
    let Some(appdata) = src.env("APPDATA") else { return Vec::new() };
    let base = format!("{}\\itch", appdata.trim_end_matches('\\'));
    let mut out = Vec::new();
    if !src.list_dir(&format!("{base}\\apps")).is_empty() {
        out.push(format!("{base}\\apps"));
    }
    if let Some(text) = src.read_file(&format!("{base}\\preferences.json")) {
        out.extend(itch_locations(&text));
    }
    out
}

/// The install locations in itch's `preferences.json`
/// (`installLocations: { id: { path } }`).
pub(crate) fn itch_locations(text: &str) -> Vec<String> {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(text) else { return Vec::new() };
    let Some(locs) = v.get("installLocations").and_then(|l| l.as_object()) else { return Vec::new() };
    locs.values().filter_map(|l| l.get("path").and_then(|p| p.as_str())).map(str::to_string).collect()
}

/// The cached folders (`collect_roots` of the live sources).
static ROOTS: RwLock<Vec<String>> = RwLock::new(Vec::new());
static REFRESHER: Once = Once::new();

/// Replace the cached folders; `true` when they changed.
pub(crate) fn store_roots(roots: Vec<String>) -> bool {
    let mut cached = ROOTS.write().unwrap_or_else(|e| e.into_inner());
    let changed = *cached != roots;
    *cached = roots;
    changed
}

/// `exe_path` lies in a known launcher's game library (Epic, GOG, Ubisoft,
/// EA, Battle.net, itch). The first call starts the background scan; until
/// it lands nothing matches.
pub(crate) fn is_launcher_library_path(exe_path: &str) -> bool {
    #[cfg(not(test))]
    REFRESHER.call_once(crate::launcher_libraries_win::start_refresher);
    #[cfg(test)]
    let _ = &REFRESHER;
    is_under(exe_path, &ROOTS.read().unwrap_or_else(|e| e.into_inner()))
}

#[cfg(test)]
#[path = "launcher_libraries_tests.rs"]
mod tests;
