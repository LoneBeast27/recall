//! Settings → Games: the apps the user registered with the recorder by hand (the maintainer
//! 2026-11-07: "use official proof + support for manual application registering"). The
//! recorder records an unknown fullscreen app only with official proof that it is a game (a
//! Steam or launcher install, an Xbox / Store game package, a game Recall or the NVIDIA driver
//! knows, a game-like render verdict); anything else is registered here.
//!
//! The list is the controller's config, read from its `config` event, never a copy:
//! - `[[overrides]]` with a game kind (`hdr_game` / `sdr_game`) = "Record" (any window, windowed
//!   too), with kind `none` = "Never record" (beats every proof, Steam included);
//! - `[detect] fullscreen_games` = "Record when fullscreen" (Discord by default, the maintainer
//!   2026-11-05).
//!
//! Changes go out one app at a time (`register_game`, R8-detect-7): the controller applies
//! each to its current config (`record_game_ops`), so two quick edits or a tray "Add as a
//! game" in between never overwrite each other, and an app already recorded keeps its class
//! and clamp (R8-detect-4). An exe is matched like the controller does (G9): case-insensitive,
//! `.exe` optional ("Game.exe" = "game"). Pure: the view (`views/settings_games.rs`) sends
//! what these build.

use serde_json::Value;

use crate::services::audio_apps::PlayingApp;
use crate::services::daemon::DaemonCommand;

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
    /// The `register_game` command's `how`.
    pub fn wire(self) -> &'static str {
        match self {
            Registration::Record => "record",
            Registration::Never => "never",
            Registration::Fullscreen => "fullscreen",
        }
    }

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

/// The command for a segment click on `entry`: `None` when `how` is already its
/// registration (no rewrite of its class or clamp, R8-detect-4).
pub fn segment_command(entry: &GameEntry, how: Registration) -> Option<DaemonCommand> {
    (entry.how != how).then(|| DaemonCommand::RegisterGame { exe: entry.exe.clone(), how: how.wire() })
}

/// "Add app…" / "Browse for an exe…": record `exe` (an app already recorded keeps its entry).
pub fn add_command(exe: &str) -> DaemonCommand {
    DaemonCommand::RegisterGame { exe: exe.trim().to_owned(), how: Registration::Record.wire() }
}

/// "Remove": its override, or its fullscreen-only entry.
pub fn remove_command(entry: &GameEntry) -> DaemonCommand {
    let how = if entry.how == Registration::Fullscreen { "unlist" } else { "remove" };
    DaemonCommand::RegisterGame { exe: entry.exe.clone(), how }
}

/// A top-level window as `EnumWindows` reports it ("Add app…").
#[derive(Clone, Copy, Debug, Default)]
pub struct WinInfo {
    pub visible: bool,
    pub owned: bool,
    pub exstyle: u32,
    /// Window rect width, height.
    pub size: (i32, i32),
}

const WS_EX_TOOLWINDOW: u32 = 0x0000_0080;
const WS_EX_APPWINDOW: u32 = 0x0004_0000;

/// An application window "Add app…" offers (R8-detect-3): visible, owner-less, not a tool
/// window (unless it asks for a taskbar button), with a non-zero size, of any style: a
/// borderless `WS_POPUP` game window (SDL's default) has no caption and no APPWINDOW bit.
pub fn is_app_window(w: &WinInfo) -> bool {
    let tool = w.exstyle & WS_EX_TOOLWINDOW != 0 && w.exstyle & WS_EX_APPWINDOW == 0;
    w.visible && !w.owned && !tool && w.size.0 > 0 && w.size.1 > 0
}

/// The apps "Add app…" lists, and whether `apps[0]` is the one that was in front.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Pickable {
    pub apps: Vec<PlayingApp>,
    pub in_front: bool,
}

/// Exes never offered by "Add app…": Recall's own and Windows itself, the same list the
/// controller refuses a "Record" entry for (R2-detect-r10-4).
#[path = "../../../recall-hdr/src/never_recorded.rs"]
mod never_recorded;

/// The running apps "Add app…" offers, in z-order: one row per exe, minus Recall and the
/// shell. The app of `front_pid` (the foreground window when the menu opened) goes first and
/// is the only one labelled "in front"; Recall itself in front labels none (R8-detect-3).
pub fn pickable(found: Vec<(u32, PlayingApp)>, front_pid: Option<u32>) -> Pickable {
    let front_exe = found.iter().find(|(pid, _)| Some(*pid) == front_pid).map(|(_, a)| a.exe.clone());
    let mut out: Vec<PlayingApp> = Vec::new();
    for (_, app) in found {
        let own = never_recorded::never_recordable(&app.exe);
        if !own && !app.exe.trim().is_empty() && !out.iter().any(|a| same_exe(&a.exe, &app.exe)) {
            out.push(app);
        }
    }
    let at = front_exe.and_then(|exe| out.iter().position(|a| same_exe(&a.exe, &exe)));
    if let Some(i) = at {
        let app = out.remove(i);
        out.insert(0, app);
    }
    Pickable { in_front: at.is_some(), apps: out }
}

#[cfg(test)]
#[path = "game_apps_tests.rs"]
mod tests;
