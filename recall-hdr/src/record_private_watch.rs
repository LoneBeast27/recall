//! The G4 notice for a window the private list blocks (R6 merge gate). A
//! fullscreen call / chat / office / media app (`private_apps`) is never
//! recorded without an opt-in, but a game can share such a name (a non-Steam
//! copy of a game called `Element.exe`). So the block is never silent:
//!
//! - every blocked fullscreen exe is logged once per run;
//! - while notices are on and the render check can run, the window is
//!   watched like an unknown fullscreen window (`record_render`), but its
//!   verdict never commits it: [`private_admit`] turns every outcome into a
//!   rejection, a "game" verdict included;
//! - when that window renders like a game (a game verdict: independent flip,
//!   exclusive fullscreen or tearing, [`private_front`]; composed frames at
//!   any rate are not enough, a video call draws so) and stays in front
//!   for over 20 s, the G4 notice names it once per exe: "Not recording
//!   <exe>: add it as a game in Settings". After that it is not watched again
//!   this run.
//!
//! Remote desktop / streaming clients and the deny lists never get here:
//! [`blocked_private`] asks for a window that only the private list stops.

use std::collections::HashSet;
use std::time::Instant;

use crate::config::Config;
use crate::record_target::{classify_event, is_target_with};
use crate::render_verdict::{Gate, RenderStats, Verdict, Why};
use crate::watcher::WatchEvent;

/// A plain fullscreen window that would be a target if its stem were not on
/// the private list: no opt-in and no game evidence (else `classify_event`
/// takes it), and not denied, `none`-overridden or Recall's own.
pub(crate) fn blocked_private(ev: &WatchEvent, cfg: &Config) -> bool {
    ev.fullscreen
        && !ev.hosted
        && crate::private_apps::is_private(&ev.exe_stem)
        && is_target_with(&ev.exe_stem, true, true, cfg)
        && classify_event(ev, cfg).is_none()
}

/// The render check may watch a blocked window: it can run for the pid,
/// notices are on (else nothing could show the result, R4-detect-2), and the
/// exe was not named yet.
/// The overlay can show a render-check result: notices on and not hidden by the overlay
/// shortcut. Off, the render check, the G4 clock and the stuck-capture notice do no work.
pub(crate) fn notices_visible(overlay: &crate::record_config::OverlayCfg) -> bool {
    overlay.shows_notices()
}

pub(crate) fn watch_wanted(can_check: bool, notices_on: bool, named: bool) -> bool {
    can_check && notices_on && !named
}

/// The verdict on a watched private window: `Err(at)` = ask again then
/// (still unsure), `Ok(why)` = reject it noting `why`. Never a commit: a
/// "game" verdict is noted as game-like for [`private_front`].
pub(crate) fn private_admit(gate: Gate, verdict: Verdict) -> Result<Why, Instant> {
    match (gate, verdict) {
        (Gate::Hold(at), _) => Err(at),
        (_, Verdict::Game(why)) | (_, Verdict::NotGame(why)) => Ok(why),
        (Gate::Reject(why), Verdict::Unsure) => Ok(why),
        (Gate::Commit, Verdict::Unsure) => Ok(Why::NoFrames),
    }
}

/// `Some(game_like)` while the G4 clock may run for a blocked window whose
/// last verdict was `last`: `game_like` only for a game verdict; composed
/// keeps the clock running but never names the exe on its own. `None` (the
/// streak resets) for video, no frames, no verdict yet, or while the desktop
/// is recorded (the window is on the clip).
pub(crate) fn private_front(last: Option<(Why, RenderStats)>, desktop: bool) -> Option<bool> {
    let (why, _stats) = last.filter(|_| !desktop)?;
    match why {
        Why::ExclusiveFullscreen | Why::IndependentFlip | Why::Tearing => Some(true),
        // A fullscreen video call draws composed at a steady rate in its own
        // pid (Zoom, Telegram): no notice telling the user to opt it in (R7-detect-2).
        Why::Composed => Some(false),
        Why::VideoSwapChain | Why::NoFrames => None,
    }
}

/// The record path's state: which window is watched, which exes were logged.
#[derive(Debug, Default)]
pub(crate) struct PrivateWatch {
    /// (pid, hwnd) of the blocked window the render check watches.
    pub window: Option<(u32, isize)>,
    logged: HashSet<String>,
}

impl PrivateWatch {
    /// `exe` is logged for the first time this run.
    pub fn first_block(&mut self, exe: &str) -> bool {
        self.logged.insert(exe.to_ascii_lowercase())
    }

    /// `(pid, hwnd)` is the watched blocked window: its verdict never commits.
    pub fn is_watched(&self, pid: u32, hwnd: isize) -> bool {
        self.window == Some((pid, hwnd))
    }
}

#[cfg(test)]
#[path = "record_private_watch_tests.rs"]
mod tests;
