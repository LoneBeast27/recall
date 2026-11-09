//! "Use your existing <Game> folder?": the answers the engine reads (`recording.game_folder_choices`,
//! `RECALL_GAME_FOLDER_CHOICES` at spawn, `set_game_folder_choices` on stdin while it runs), the
//! one-per-run `game_folder_exists` question, and the app's linked (Folder Mapper) folders,
//! which are never adopted.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

use super::rule::{self, Choice};

/// `RECALL_GAME_FOLDER_CHOICES`: compact JSON, `{"<folder name, lowercase>": "merge" | "separate"}`.
pub const CHOICES_ENV: &str = "RECALL_GAME_FOLDER_CHOICES";

/// `{key: "merge" | "separate"}` text -> the answers; other values and bad JSON: none.
pub fn parse_choices(text: &str) -> HashMap<String, Choice> {
    let map: BTreeMap<String, String> = serde_json::from_str(text).unwrap_or_default();
    from_map(map)
}

fn from_map(map: BTreeMap<String, String>) -> HashMap<String, Choice> {
    map.into_iter().filter_map(|(k, v)| Choice::parse(&v).map(|c| (rule::choice_key(&k), c))).collect()
}

fn live() -> &'static Mutex<Option<HashMap<String, Choice>>> {
    static LIVE: OnceLock<Mutex<Option<HashMap<String, Choice>>>> = OnceLock::new();
    LIVE.get_or_init(Default::default)
}

/// `set_game_folder_choices` (the controller, after the user answered or reset): replaces the
/// answers read from the environment for the rest of the run. No restart, the buffer stays.
pub fn set_choices(map: BTreeMap<String, String>) {
    *live().lock().unwrap_or_else(|p| p.into_inner()) = Some(from_map(map));
}

/// The answer for the folder named `name`, if the user gave one.
pub fn choice_for(name: &str) -> Option<Choice> {
    let key = rule::choice_key(name);
    if let Some(map) = live().lock().unwrap_or_else(|p| p.into_inner()).as_ref() {
        return map.get(&key).copied();
    }
    parse_choices(&std::env::var(CHOICES_ENV).unwrap_or_default()).get(&key).copied()
}

/// True the first time for `(out_dir, name)` in this run: one question per game per run.
pub fn first_ask(out_dir: &str, name: &str) -> bool {
    static ASKED: OnceLock<Mutex<HashSet<(String, String)>>> = OnceLock::new();
    let key = (out_dir.trim_end_matches(['\\', '/']).to_lowercase(), rule::choice_key(name));
    ASKED.get_or_init(Default::default).lock().unwrap_or_else(|p| p.into_inner()).insert(key)
}

/// The app's linked folders (`imported` in `%APPDATA%\Recall\folders.json`, `RECALL_FOLDERS`
/// overrides). Unit tests never read the user's file.
pub fn linked_folders() -> Vec<String> {
    if cfg!(test) {
        return Vec::new();
    }
    let file = std::env::var_os("RECALL_FOLDERS")
        .filter(|p| !p.is_empty())
        .map(PathBuf::from)
        .or_else(|| crate::user_dirs::appdata().map(|root| crate::user_dirs::checked(root.join("Recall").join("folders.json"))));
    let Some(bytes) = file.and_then(|f| std::fs::read(f).ok()) else { return Vec::new() };
    let Ok(value) = serde_json::from_slice::<serde_json::Value>(&bytes) else { return Vec::new() };
    let list = value.get("imported").and_then(|v| v.as_array());
    list.into_iter().flatten().filter_map(|v| v.as_str()).map(str::to_owned).collect()
}

fn plain(p: &Path) -> String {
    p.to_string_lossy().replace('/', "\\").trim_end_matches('\\').to_lowercase()
}

/// `dir` is a linked folder or inside one, where that link is not `out` or a parent of it (a
/// link above the clip folder keeps Recall's clips Recall's; one at a game folder is the
/// link's). The app's `under_link` rule.
pub fn under_link(dir: &Path, out: &Path, linked: &[String]) -> bool {
    let at_or_in = |p: &str, link: &str| p == link || (p.starts_with(link) && p.as_bytes().get(link.len()) == Some(&b'\\'));
    let (dir, out) = (plain(dir), plain(out));
    linked.iter().map(|l| plain(Path::new(l))).any(|link| at_or_in(&dir, &link) && !at_or_in(&out, &link))
}
