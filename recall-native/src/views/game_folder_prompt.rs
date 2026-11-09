//! "Use your existing <Game> folder?": with "Sort clips into game folders" on, the recorder found
//! a folder with the game's name in Recordings that Recall did not make (ShadowPlay's, say). It
//! saves those clips straight in Recordings and sends `game_folder_exists` once a run; the app
//! asks once per folder and stores the answer in `recording.game_folder_choices`.
//!
//! The questions still open live in a small file (`%APPDATA%\Recall\game-folder-prompts.json`),
//! so one asked while the app was closed shows on the next start. The text and the choice keys
//! are pure functions below.
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use gpui::{AnyElement, Context, FontWeight, div, prelude::*, px};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::{
    root::{Root, ToastKind},
    services::library::folders::{rule, under_link},
    theme::{self, c, ca},
    ui,
};

/// One open question: the game (the recorder's source name) and the folder it found.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Pending {
    pub game: String,
    pub folder: String,
}

/// The config key under `recording.game_folder_choices` for `folder`: its name, lowercase
/// (`game_folder_rule::choice_key`, the key the recorder reads).
pub fn choice_key(folder: &str) -> Option<String> {
    Path::new(folder).file_name().map(|n| rule::choice_key(&n.to_string_lossy()))
}

/// The answers so far (`recording.game_folder_choices`): only `merge` and `separate` count.
pub fn choices(value: Option<&Value>) -> BTreeMap<String, String> {
    let Some(map) = value.and_then(Value::as_object) else { return BTreeMap::new() };
    map.iter()
        .filter_map(|(k, v)| v.as_str().filter(|v| rule::Choice::parse(v).is_some()).map(|v| (k.clone(), v.to_string())))
        .collect()
}

/// The patch value that clears every answer: each key set to "" (the recorder drops values
/// other than `merge` and `separate`; a merge patch cannot remove a key).
pub fn reset_value(choices: &BTreeMap<String, String>) -> Value {
    Value::Object(choices.keys().map(|k| (k.clone(), json!(""))).collect())
}

/// The patch value for one answer.
pub fn answer_value(folder: &str, merge: bool) -> Option<Value> {
    let key = choice_key(folder)?;
    Some(json!({ key: if merge { rule::Choice::Merge.as_str() } else { rule::Choice::Separate.as_str() } }))
}

pub fn title(game: &str) -> String {
    format!("Use your existing {game} folder?")
}

/// The body, one paragraph per line.
pub fn body(game: &str, name: &str) -> [String; 2] {
    [
        format!(
            "Recordings already has a folder called {name} (maybe from ShadowPlay or another app). Recall can save new {game} clips into it, or keep them in a separate \"{name} (Recall)\" folder. Recall-named clips in a folder it uses count toward your storage limit."
        ),
        "Recall's storage limit only applies to new clips it saves there; your existing files are never removed.".to_string(),
    ]
}

pub const USE_FOLDER: &str = "Use this folder";
pub const KEEP_SEPARATE: &str = "Keep separate";
/// Settings, under the game-folders switch, while answers exist.
pub const RESET_LABEL: &str = "Reset folder choices (folders Recall already uses keep their clips)";

/// Whether `p` is still a question: its folder sits directly in `recordings`, has no answer
/// yet, is a real folder (not a link, not hidden or system) without a marker, and is not a
/// linked (Folder Mapper) folder. Looks at the disk.
pub fn still_open(p: &Pending, recordings: &Path, linked: &[String], answered: &BTreeMap<String, String>) -> bool {
    let folder = Path::new(&p.folder);
    let in_recordings = folder.parent().is_some_and(|parent| same(parent, recordings));
    in_recordings
        && choice_key(&p.folder).is_some_and(|k| !answered.contains_key(&k))
        && rule::is_plain_dir(folder)
        && !rule::has_marker(folder)
        && !under_link(folder, recordings, linked)
}

fn same(a: &Path, b: &Path) -> bool {
    let plain = |p: &Path| p.to_string_lossy().replace('/', "\\").trim_end_matches('\\').to_lowercase();
    plain(a) == plain(b)
}

/// `list` plus `p`, unless a question for the same folder is already in it.
pub fn add(list: &mut Vec<Pending>, p: Pending) -> bool {
    let key = choice_key(&p.folder);
    if key.is_none() || list.iter().any(|q| choice_key(&q.folder) == key) {
        return false;
    }
    list.push(p);
    true
}

/// The answer to save for `folder`, or why there is none. "Use this folder" adopts it first
/// (`adopt`: the marker; the files there stay the user's), so the library lists it before the
/// next save. A failed adoption is `Err` (the toast's text): no answer is saved and the
/// question stays open. `Ok(None)`: a folder with no name to key on.
pub(crate) fn answer(folder: &str, merge: bool, adopt: impl FnOnce(&Path) -> std::io::Result<()>) -> Result<Option<Value>, String> {
    let Some(value) = answer_value(folder, merge) else { return Ok(None) };
    if merge {
        adopt(Path::new(folder)).map_err(|e| e.to_string())?;
    }
    Ok(Some(value))
}

/// The open-questions file: `RECALL_GAME_FOLDER_PROMPTS`, else beside `RECALL_LIBRARY_META`
/// (a sandboxed run's app data), else `%APPDATA%\Recall\game-folder-prompts.json`.
fn prompts_path() -> Option<PathBuf> {
    prompts_path_from(std::env::var_os("RECALL_GAME_FOLDER_PROMPTS"), std::env::var_os("RECALL_LIBRARY_META"), crate::user_dirs::appdata())
}

/// [`prompts_path`] from its inputs. Every result passes `user_dirs::checked`, so a unit test
/// that reaches the real profile panics.
pub(crate) fn prompts_path_from(own: Option<std::ffi::OsString>, library: Option<std::ffi::OsString>, appdata: Option<PathBuf>) -> Option<PathBuf> {
    const FILE: &str = "game-folder-prompts.json";
    let path = if let Some(own) = own.filter(|p| !p.is_empty()) {
        PathBuf::from(own)
    } else if let Some(library) = library.filter(|p| !p.is_empty()) {
        PathBuf::from(library).with_file_name(FILE)
    } else {
        appdata?.join("Recall").join(FILE)
    };
    Some(crate::user_dirs::checked(path))
}

pub fn load_from(path: &Path) -> Vec<Pending> {
    std::fs::read(path).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default()
}

pub fn save_to(path: &Path, list: &[Pending]) -> Result<(), String> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    let bytes = serde_json::to_vec_pretty(list).map_err(|e| e.to_string())?;
    crate::services::durable::write_durable(path, &bytes)
}

/// At start: the questions left open, without folders that are gone, marked or no longer a
/// real folder (the answers are checked once the config arrives: [`Root::prune_game_folder_prompts`]).
pub fn load_open() -> Vec<Pending> {
    let mut list = prompts_path().map(|p| load_from(&p)).unwrap_or_default();
    list.retain(|p| rule::is_plain_dir(Path::new(&p.folder)) && !rule::has_marker(Path::new(&p.folder)));
    list
}

fn save(list: &[Pending]) {
    if let Some(Err(e)) = prompts_path().map(|path| save_to(&path, list)) {
        eprintln!("game folders: open questions not saved ({e})");
    }
}

impl Root {
    fn answered_game_folders(&self) -> BTreeMap<String, String> {
        choices(self.config_value("recording", "game_folder_choices"))
    }

    /// Drops questions that are no longer open ([`still_open`]) and saves the list. Needs the
    /// config (the answers): does nothing before it arrives.
    pub fn prune_game_folder_prompts(&mut self) {
        if self.snap.config.is_none() {
            return;
        }
        let (recordings, answered) = (PathBuf::from(self.recordings_folder()), self.answered_game_folders());
        let before = self.game_folder_prompts.len();
        let linked: Vec<String> = self.folders.linked().map(str::to_owned).collect();
        self.game_folder_prompts.retain(|p| still_open(p, &recordings, &linked, &answered));
        if self.game_folder_prompts.len() != before {
            save(&self.game_folder_prompts);
        }
    }

    /// `game_folder_exists` from the recorder.
    pub fn on_game_folder_exists(&mut self, game: String, folder: String) {
        if add(&mut self.game_folder_prompts, Pending { game, folder }) {
            save(&self.game_folder_prompts);
        }
        self.prune_game_folder_prompts();
    }

    fn answer_game_folder(&mut self, p: Pending, merge: bool) {
        let value = match answer(&p.folder, merge, rule::adopt) {
            Ok(Some(value)) => value,
            Ok(None) => return,
            Err(e) => return self.show_toast(ToastKind::Bad, "Couldn't use that folder", Some(e)),
        };
        if merge {
            crate::services::library::folders::forget_scans();
            self.workers.job(crate::worker::Job::List);
        }
        self.game_folder_prompts.retain(|q| q != &p);
        save(&self.game_folder_prompts);
        self.set_config("recording", "game_folder_choices", value);
    }

    pub fn render_game_folder_prompt(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let answered = self.answered_game_folders();
        let p = self
            .game_folder_prompts
            .iter()
            .find(|p| self.snap.config.is_some() && choice_key(&p.folder).is_some_and(|k| !answered.contains_key(&k)))?
            .clone();
        let name = Path::new(&p.folder).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let [first, second] = body(&p.game, &name);
        let (merge, keep) = (p.clone(), p.clone());
        let actions = div()
            .flex()
            .gap(px(8.))
            .justify_end()
            .child(ui::button("game-folder-separate", ui::Variant::Secondary).child(KEEP_SEPARATE).on_click(cx.listener(move |this, _, _, cx| {
                this.answer_game_folder(keep.clone(), false);
                cx.notify();
            })))
            .child(ui::button("game-folder-merge", ui::Variant::Primary).child(USE_FOLDER).on_click(cx.listener(move |this, _, _, cx| {
                this.answer_game_folder(merge.clone(), true);
                cx.notify();
            })));
        let text = |t: String| div().text_color(c(theme::MUTED)).child(t);
        let card = ui::panel()
            .w(px(480.))
            .p(px(20.))
            .flex()
            .flex_col()
            .gap(px(14.))
            .bg(c(theme::RAISED))
            .border_color(c(theme::BORDER))
            .child(
                div()
                    .font_family(theme::display())
                    .text_size(px(18.))
                    .line_height(px(24.))
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(title(&p.game)),
            )
            .child(text(first))
            .child(text(second))
            .child(actions);
        Some(
            div()
                .id("game-folder-scrim")
                .absolute()
                .inset_0()
                .flex()
                .items_center()
                .justify_center()
                .bg(ca(0x000000, 0.55))
                // The scrim takes the press so the page under it does not act.
                .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .child(card)
                .into_any_element(),
        )
    }
}

#[cfg(test)]
#[path = "game_folder_prompt_tests.rs"]
mod tests;
