//! Transition rules for the capture target (transitions audit 2026-11-06):
//! which window may become, or replace, the recorded game. Pure logic; the
//! tracker (`record_target`) asks these, the controller feeds them.
//!
//! - **Traits** ([`traits_of`]): what a candidate is beyond its window:
//!   fullscreen, vouched for by name (an override, a known game, or a new
//!   window of a game whose window was just destroyed), or only a
//!   `detect.fullscreen_games` entry (Discord).
//! - **Drain** (T2): while the closed game's last clip is still saveable, a
//!   windowed Steam-library window (a crash reporter, the game's own launcher,
//!   the same process's included) and a `fullscreen_games` entry wait until
//!   the drain ends; a fullscreen or vouched-for window may end it, and the
//!   island says so ([`drain_cut`]: "Last clip dropped"), unless it is a pin.
//! - **Listed only** (T3): a `fullscreen_games` entry may start a session but
//!   never replace a live one; it commits once the session ends.
//! - **Adoption** (T5a): the session's window was destroyed but its process
//!   lives (a fullscreen <-> windowed switch re-makes the window): for
//!   [`ADOPT_FOR`] that process's next foreground window is the session,
//!   windowed or not, when fullscreen or at least [`MIN_CLIENT`]
//!   ([`adopted_candidate`]).
//! - **Cool-down** (T9): after one live game replaced another, the session
//!   cannot be replaced by another live game for [`REPLACE_COOLDOWN`], unless
//!   its process exits or its window goes.
//! - **Windowed start** (T10): with no session, a windowed Steam-library
//!   window needs [`WINDOWED_START`] and a client area of at least
//!   [`MIN_CLIENT`] ([`too_small`]): a splash or launcher window does not
//!   start the engine. Fullscreen keeps the 300 ms start. A windowed known
//!   game keeps the 300 ms start but needs the same size (R2-4).

use std::time::Duration;

use crate::config::Config;
use crate::record_engine::EngineTarget;
use crate::record_target::{is_fullscreen_game, is_target, Target};
use crate::rules;
use crate::watcher::WatchEvent;

/// How long a windowed Steam-library window must hold the foreground before
/// it starts a session from nothing.
pub const WINDOWED_START: Duration = Duration::from_millis(1500);
/// The smallest client area (width, height) a windowed Steam-library window
/// needs to be a candidate at all.
pub const MIN_CLIENT: (i32, i32) = (640, 360);
/// After a retarget between two live games, how long the new session cannot
/// be replaced by another live game.
pub const REPLACE_COOLDOWN: Duration = Duration::from_secs(10);
/// How long a process whose session window was destroyed stays adoptable.
pub const ADOPT_FOR: Duration = Duration::from_secs(10);

/// What a candidate is, beyond its window.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Traits {
    /// Fullscreen / borderless on its monitor.
    pub fullscreen: bool,
    /// A game by name: an override, a known game, or adopted.
    pub vouched: bool,
    /// Only a `detect.fullscreen_games` entry: starts a session, never
    /// replaces a live one.
    pub listed_only: bool,
    /// Vouched for only by `known_games` (no override): windowed, it still
    /// needs [`MIN_CLIENT`], so the game's own error box is no candidate (R2-4).
    pub known_only: bool,
}

impl Traits {
    /// A plain fullscreen window (what `TargetTracker::on_foreground` assumes).
    pub const FULLSCREEN: Traits = Traits { fullscreen: true, vouched: false, listed_only: false, known_only: false };
    /// A windowed Steam-library window on the look alone.
    pub const WINDOWED: Traits = Traits { fullscreen: false, vouched: false, listed_only: false, known_only: false };
    /// A new window of the process whose session window was destroyed.
    pub const ADOPTED: Traits = Traits { fullscreen: false, vouched: true, listed_only: false, known_only: false };

    /// A windowed window that is a candidate only by its Steam-library path.
    pub fn windowed_only(self) -> bool {
        !self.fullscreen && !self.vouched
    }
}

/// The traits of a foreground event that `classify_event` made a candidate.
pub fn traits_of(ev: &WatchEvent, cfg: &Config) -> Traits {
    let over = rules::override_kind(cfg, &ev.exe_stem).is_some();
    let known = crate::known_games::is_known_game(&ev.exe_stem, &ev.title);
    let listed = ev.fullscreen && is_fullscreen_game(cfg, &ev.exe_stem);
    Traits {
        fullscreen: ev.fullscreen,
        vouched: over || known,
        listed_only: listed && !over && !known && !ev.steam_game,
        known_only: known && !over,
    }
}

/// A windowed candidate that needs [`MIN_CLIENT`] (a windowed-only Steam
/// window, or a known game's window with no override: the 300 ms start
/// stays, the floor applies, R2-4) whose client area is under it.
pub fn too_small(traits: Traits, client: (i32, i32)) -> bool {
    let sized = traits.windowed_only() || (!traits.fullscreen && traits.known_only);
    sized && (client.0 < MIN_CLIENT.0 || client.1 < MIN_CLIENT.1)
}

/// A foreground window of an adoptable process: the session when it is
/// fullscreen or at least [`MIN_CLIENT`] (an error MessageBox or a small
/// launcher of the same process is not), unless the stem is never a target
/// (denied, a `none` override, Recall itself).
pub fn adopted_candidate(ev: &WatchEvent, cfg: &Config, client: (i32, i32)) -> Option<Target> {
    let big = ev.fullscreen || (client.0 >= MIN_CLIENT.0 && client.1 >= MIN_CLIENT.1);
    (big && is_target(&ev.exe_stem, true, cfg)).then(|| Target::from_event(ev))
}

/// The window that started when a retarget cut a drain short (any other
/// window, the same game's re-made one included: the engine restarts and its
/// ring with it): `Some(its name)` for the island notice. A pin is the
/// player's own choice: no notice.
pub fn drain_cut(drained: Option<&Target>, applied: &EngineTarget, pinned: Option<&Target>) -> Option<String> {
    match (drained, applied) {
        (_, EngineTarget::Window(b)) if pinned == Some(b) => None,
        (Some(a), EngineTarget::Window(b)) if (a.pid, a.hwnd) != (b.pid, b.hwnd) => Some(b.name.clone()),
        _ => None,
    }
}

#[cfg(test)]
#[path = "record_target_transition_tests.rs"]
mod tests;
