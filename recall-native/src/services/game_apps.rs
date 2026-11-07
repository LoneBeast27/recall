//! Settings → Games: the apps the user registered with the recorder by hand (Rohan
//! 2026-11-07: "use official proof + support for manual application registering"). The
//! recorder records an unknown fullscreen app only with official proof that it is a game (a
//! Steam or launcher install, an Xbox / Store game package, a game Recall or the NVIDIA driver
//! knows, a game-like render verdict); anything else is registered here.
//!
//! The list is the controller's config, read from its `config` event, never a copy:
//! - `[[overrides]]` with a game kind (`hdr_game` / `sdr_game`) = "Record" (any window, windowed
//!   too), with kind `none` = "Never record" (beats every proof, Steam included);
//! - `[detect] fullscreen_games` = "Record when fullscreen" (Discord by default, Rohan
//!   2026-11-05).
//!
//! Changes go out as `set_config` patches through the existing patch path: the whole
//! `overrides` array (the controller replaces arrays) or `detect.fullscreen_games`. An exe is
//! matched like the controller does (G9): case-insensitive, `.exe` optional ("Game.exe" =
//! "game"). Pure: the view (`views/settings_games.rs`) sends what these build.

use serde_json::{Value, json};

use crate::services::audio_apps::PlayingApp;

/// How an app is registered.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Registration {
    /// A game override: recorded whenever it is in front.
    Record,
    /// A `none` override: never recorded.
    Never,
    /// A `detect.fullscreen_games` entry: recorded while fullscreen.
    Fullscreen,
}

impl Registration {
    pub fn label(self) -> &'static str {
        match self {
            Registration::Record => "Recorded",
            Registration::Never => "Never recorded",
            Registration::Fullscreen => "Recorded when fullscreen",
        }
    }
}

/// One row of the Games list.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GameEntry {
    /// As written in the config (`Game.exe` or `game`).
    pub exe: String,
    pub how: Registration,
}

/// `Game.exe` → `Game`; a bare stem is unchanged.
fn stem(exe: &str) -> &str {
    let exe = exe.trim();
    match exe.len().checked_sub(4) {
        Some(i) if exe.is_char_boundary(i) && exe[i..].eq_ignore_ascii_case(".exe") => &exe[..i],
        _ => exe,
    }
}

/// `a` and `b` name the same exe (G9).
pub fn same_exe(a: &str, b: &str) -> bool {
    !stem(a).is_empty() && stem(a).eq_ignore_ascii_case(stem(b))
}

fn overrides(config: &Value) -> Vec<Value> {
    config.get("overrides").and_then(Value::as_array).cloned().unwrap_or_default()
}

fn fullscreen_games(config: &Value) -> Vec<String> {
    let list = config.get("detect").and_then(|d| d.get("fullscreen_games")).and_then(Value::as_array);
    list.map(|l| l.iter().filter_map(Value::as_str).map(str::to_owned).collect()).unwrap_or_default()
}

/// The registered apps: overrides first (in config order), then the fullscreen-only entries.
pub fn entries(config: Option<&Value>) -> Vec<GameEntry> {
    let Some(config) = config else { return Vec::new() };
    let mut out: Vec<GameEntry> = overrides(config)
        .iter()
        .filter_map(|o| {
            let exe = o.get("exe")?.as_str()?.trim().to_owned();
            let how = if o.get("kind")?.as_str()? == "none" { Registration::Never } else { Registration::Record };
            (!exe.is_empty()).then_some(GameEntry { exe, how })
        })
        .collect();
    for exe in fullscreen_games(config) {
        out.push(GameEntry { exe, how: Registration::Fullscreen });
    }
    out
}

/// The game kind a "Record" override gets: the config's default game class, else `sdr_game`.
fn game_kind(config: &Value) -> &'static str {
    match config.get("behavior").and_then(|b| b.get("default_game_class")).and_then(Value::as_str) {
        Some("hdr_game") => "hdr_game",
        _ => "sdr_game",
    }
}

/// The patch that registers `exe` (`Record` or `Never`) as an override, replacing any override
/// of the same exe and keeping every other one (and its clamp) as it is. `Fullscreen` adds a
/// `detect.fullscreen_games` entry instead.
pub fn register_patch(config: &Value, exe: &str, how: Registration) -> Value {
    let exe = exe.trim();
    if how == Registration::Fullscreen {
        let mut list = fullscreen_games(config);
        if !list.iter().any(|g| same_exe(g, exe)) {
            list.push(exe.to_owned());
        }
        return json!({ "detect": { "fullscreen_games": list } });
    }
    let kind = if how == Registration::Never { "none" } else { game_kind(config) };
    let mut list: Vec<Value> = overrides(config).into_iter().filter(|o| !o.get("exe").and_then(Value::as_str).is_some_and(|e| same_exe(e, exe))).collect();
    list.push(json!({ "exe": exe, "kind": kind }));
    json!({ "overrides": list })
}

/// The patch that removes `entry` (its override, or its fullscreen-only entry).
pub fn remove_patch(config: &Value, entry: &GameEntry) -> Value {
    if entry.how == Registration::Fullscreen {
        let list: Vec<String> = fullscreen_games(config).into_iter().filter(|g| !same_exe(g, &entry.exe)).collect();
        return json!({ "detect": { "fullscreen_games": list } });
    }
    let list: Vec<Value> = overrides(config).into_iter().filter(|o| !o.get("exe").and_then(Value::as_str).is_some_and(|e| same_exe(e, &entry.exe))).collect();
    json!({ "overrides": list })
}

/// Exes never offered by "Add app…": Recall's own, and shell / frame processes whose windows
/// are not the app itself.
const NOT_OFFERED: &[&str] = &[
    "recall", "recall-native", "recall-hdr", "recall-hub", "recall-overlay", "explorer", "applicationframehost", "textinputhost",
    "shellexperiencehost", "startmenuexperiencehost", "searchhost", "searchapp", "lockapp", "systemsettings",
];

/// The running apps "Add app…" offers, in the order found (front-most window first): one row
/// per exe, minus Recall and the shell. The first one is the app that was in front.
pub fn pickable(found: Vec<PlayingApp>) -> Vec<PlayingApp> {
    let mut out: Vec<PlayingApp> = Vec::new();
    for app in found {
        let own = NOT_OFFERED.iter().any(|n| same_exe(n, &app.exe));
        if !own && !app.exe.trim().is_empty() && !out.iter().any(|a| same_exe(&a.exe, &app.exe)) {
            out.push(app);
        }
    }
    out
}

#[cfg(test)]
#[path = "game_apps_tests.rs"]
mod tests;
