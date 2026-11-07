//! Capture-target selection for the unified record mode. Pure logic, no Win32:
//! the pump feeds [`WatchEvent`]s in and reads the target out (what the engine
//! then does with it lives in `record_engine`).
//!
//! - **Classification** ([`classify_event`]): a target is a foreground window
//!   that is fullscreen/borderless on its own monitor, an exe in a Steam library
//!   (windowed games), a known game (`known_games`, windowed too), or an exe
//!   with an `hdr_game|sdr_game` override; never a screensaver (`.scr`), a
//!   denied stem (the rules deny list,
//!   Recall's own processes, explorer, browsers, media players, overlays,
//!   launchers) or an exe with a `none` override. A window that is a
//!   candidate by the fullscreen rule alone must also render like a game
//!   before it commits (`record_render`, asked through [`Admit`]).
//! - **Session** ([`TargetTracker`]): the same rules as the HDR `Reconciler`
//!   session, applied to capture. With nothing recorded (no session, or the
//!   engine lingering after a game exited) a candidate commits once it holds
//!   the foreground for [`START_DEBOUNCE`] (300 ms): the first game starts
//!   recording almost at once. A *different* game (another process) replaces a
//!   live session only after [`TARGET_DEBOUNCE`] (2 s), so a brief alt-tab to
//!   another game does not restart the engine; a new window of the session's
//!   own process (a fullscreen mode switch) needs only [`START_DEBOUNCE`]. The
//!   deadline follows the session: if the session ends while a candidate
//!   waits, the short debounce applies from when that candidate was first
//!   seen. A non-target foreground (alt-tab) never ends the session; the
//!   session ends when its process exits or its window is destroyed.
//! - **Transitions** (`record_target_gate`): during the closed game's drain a
//!   windowed Steam-library window waits; a `fullscreen_games` entry never
//!   replaces a live session; a re-made window of the session's process is
//!   adopted; a 10 s cool-down after a retarget between live games; with no
//!   session a windowed Steam-library window needs 1.5 s and 640x360.

use std::time::{Duration, Instant};

use crate::record_target_gate::{Traits, ADOPT_FOR, REPLACE_COOLDOWN, WINDOWED_START};
#[cfg(test)]
pub use crate::record_target_rules::is_target;
pub use crate::record_target_rules::{classify_event, followable, game_evidence, is_fullscreen_game, is_target_with, skips_render_check};
use crate::watcher::WatchEvent;

/// How long a different game must hold the foreground before it replaces a
/// live session (alt-tab protection).
pub const TARGET_DEBOUNCE: Duration = Duration::from_secs(2);
/// How long a candidate must hold the foreground when nothing is recorded, or
/// when it is a new window of the session's own process.
pub const START_DEBOUNCE: Duration = Duration::from_millis(300);
/// Source name of the idle monitor capture.
pub const DESKTOP_NAME: &str = "Desktop";


/// A window the engine can capture. `exe` is the image file name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Target {
    pub hwnd: isize,
    pub pid: u32,
    pub name: String,
    pub exe: String,
}

impl Target {
    pub fn from_event(ev: &WatchEvent) -> Target {
        Target {
            hwnd: ev.hwnd,
            pid: ev.pid,
            name: display_name(&ev.title, &ev.exe_stem),
            exe: ev.exe_name.clone(),
        }
    }

    fn same_window(&self, other: &Target) -> bool {
        self.pid == other.pid && self.hwnd == other.hwnd
    }
}

/// Collapse whitespace, drop control characters, cap at 64 chars.
pub fn sanitize(s: &str) -> String {
    let cleaned: String = s.chars().map(|c| if c.is_control() { ' ' } else { c }).collect();
    let collapsed = cleaned.split_whitespace().collect::<Vec<_>>().join(" ");
    collapsed.chars().take(64).collect()
}

/// Source name: the sanitized window title, else the exe stem, else "Game".
pub fn display_name(title: &str, exe_stem: &str) -> String {
    let t = sanitize(title);
    if !t.is_empty() {
        return t;
    }
    let s = sanitize(exe_stem);
    if s.is_empty() { "Game".into() } else { s }
}


/// The debounced capture session (see the module docs).
pub struct TargetTracker {
    /// Replacing a live session with a different game.
    debounce: Duration,
    /// Starting a session from nothing, or a new window of the session's process.
    start: Duration,
    current: Option<Target>,
    /// The session's window was fullscreen when last seen in front (G5).
    current_fullscreen: bool,
    /// A foreground observation and when it was first seen; `None` inside = a
    /// non-target foreground (it cancels a pending game, never the session).
    pending: Option<(Option<Target>, Instant)>,
    /// How long the last committed candidate waited (first seen -> commit).
    last_wait: Option<Duration>,
    /// The render check asked to decide the pending candidate later.
    held: Option<Instant>,
    /// What the pending candidate is (`record_target_gate`).
    traits: Traits,
    /// The closed game's pid while its drain runs (T2).
    draining: Option<u32>,
    /// When a live game last replaced another (T9 cool-down).
    replaced_at: Option<Instant>,
    /// A live process whose session window was destroyed, and when (T5a).
    adoptable: Option<(u32, Instant)>,
}

/// What the render check says about a candidate that fell due.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Admit {
    Commit,
    /// Ask again at this time; the candidate stays pending.
    Hold(Instant),
    /// Not a game: drop the candidate (the session, if any, stays).
    Reject,
}

impl TargetTracker {
    pub fn new(debounce: Duration, start: Duration) -> Self {
        TargetTracker {
            debounce,
            start,
            current: None,
            current_fullscreen: false,
            pending: None,
            last_wait: None,
            held: None,
            traits: Traits::FULLSCREEN,
            draining: None,
            replaced_at: None,
            adoptable: None,
        }
    }

    pub fn current(&self) -> Option<&Target> {
        self.current.as_ref()
    }

    /// The hold a candidate needs against the current session.
    fn hold_for(&self, candidate: &Option<Target>) -> Duration {
        match (candidate, &self.current) {
            (Some(c), Some(cur)) if c.pid != cur.pid => self.debounce,
            (Some(_), None) if self.traits.windowed_only() => WINDOWED_START,
            _ => self.start,
        }
    }

    /// When the pending candidate may commit; `None` while it waits for the
    /// session or the drain to end (`record_target_gate`).
    pub fn next_deadline(&self) -> Option<Instant> {
        let (candidate, seen) = self.pending.as_ref()?;
        let mut due = *seen + self.hold_for(candidate);
        match (candidate, &self.current) {
            (Some(c), Some(cur)) if c.pid != cur.pid => {
                // T3: a `fullscreen_games` entry, and G5: a windowed
                // Steam-library window (Blender, Krita) over a fullscreen
                // session, wait for the session to end.
                if self.traits.listed_only || (self.traits.windowed_only() && self.current_fullscreen) {
                    return None;
                }
                if let Some(at) = self.replaced_at {
                    due = due.max(at + REPLACE_COOLDOWN);
                }
            }
            // The drain (T2): any windowed-only window waits, the same process's
            // too (an adopted re-made window is vouched for); so does a
            // `fullscreen_games` entry.
            (Some(_), None) if (self.traits.windowed_only() || self.traits.listed_only) && self.draining.is_some() => return None,
            _ => {}
        }
        Some(self.held.map_or(due, |held| held.max(due)))
    }

    /// The pending candidate, if it is a window (not an alt-tab away).
    pub fn pending(&self) -> Option<&Target> {
        self.pending.as_ref().and_then(|(c, _)| c.as_ref())
    }

    /// A foreground change was observed (`None` = not a target). Seeing the
    /// pending candidate again keeps its first-seen time.
    pub fn on_foreground(&mut self, candidate: Option<Target>, now: Instant) {
        self.on_foreground_with(candidate, Traits::FULLSCREEN, now)
    }

    /// [`Self::on_foreground`] with what the candidate is (`record_target_gate`).
    pub fn on_foreground_with(&mut self, candidate: Option<Target>, traits: Traits, now: Instant) {
        match (&candidate, &self.current, &self.pending) {
            // Re-focusing the live target: nothing pending, nothing to do.
            (Some(c), Some(cur), _) if c.same_window(cur) => {
                self.pending = None;
                self.current_fullscreen = traits.fullscreen;
            }
            (Some(c), _, Some((Some(p), _))) if c.same_window(p) => self.traits = traits,
            (None, _, Some((None, _))) => {}
            _ => {
                self.pending = Some((candidate, now));
                self.held = None;
                self.traits = traits;
            }
        }
    }

    /// Commit a settled candidate. Returns `true` when the target changed.
    pub fn poll(&mut self, now: Instant) -> bool {
        self.poll_admit(now, |_| Admit::Commit)
    }

    /// [`Self::poll`], asking `admit` about a due candidate before it commits.
    pub fn poll_admit(&mut self, now: Instant, admit: impl FnOnce(&Target) -> Admit) -> bool {
        let due = self.next_deadline().is_some_and(|d| now >= d);
        if !due {
            return false;
        }
        let Some((candidate, seen)) = self.pending.take() else { return false };
        self.held = None;
        match candidate {
            // Alt-tab / desktop: the session persists.
            None => false,
            Some(c) if self.current.as_ref().is_some_and(|cur| cur.same_window(&c)) => false,
            Some(c) => match admit(&c) {
                Admit::Commit => {
                    if self.current.as_ref().is_some_and(|cur| cur.pid != c.pid) {
                        self.replaced_at = Some(now);
                    }
                    self.adoptable = None;
                    self.current = Some(c);
                    self.current_fullscreen = self.traits.fullscreen;
                    self.last_wait = Some(now.duration_since(seen));
                    true
                }
                Admit::Hold(at) => {
                    self.pending = Some((Some(c), seen));
                    self.held = Some(at);
                    false
                }
                Admit::Reject => false,
            },
        }
    }

    /// The closed game's drain: `Some(its pid)` while its last clip is still
    /// saveable (or about to be: its session just ended).
    pub fn set_draining(&mut self, pid: Option<u32>) {
        self.draining = pid;
    }

    /// `pid`'s session window was destroyed under [`ADOPT_FOR`] ago while it
    /// lives: its next foreground window is the session.
    pub fn adopts(&self, pid: u32, now: Instant) -> bool {
        self.adoptable.is_some_and(|(p, at)| p == pid && now < at + ADOPT_FOR)
    }

    /// How long the last committed target waited from first seen to commit
    /// (taken once, for the retarget log line).
    pub fn take_last_wait(&mut self) -> Option<Duration> {
        self.last_wait.take()
    }

    /// The process `pid` exited: end its session (and drop a pending candidate
    /// for it). Returns `true` when the target changed.
    pub fn on_process_exit(&mut self, pid: u32) -> bool {
        if matches!(&self.pending, Some((Some(c), _)) if c.pid == pid) {
            self.pending = None;
            self.held = None;
        }
        if self.adoptable.is_some_and(|(p, _)| p == pid) {
            self.adoptable = None;
        }
        if self.current.as_ref().is_some_and(|c| c.pid == pid) {
            self.current = None;
            self.replaced_at = None;
            return true;
        }
        false
    }

    /// The target's window was destroyed while its process lives on (e.g. the
    /// game dropped back to an in-process launcher): end the session so the
    /// engine is not left capturing a dead window. Returns `true` on change.
    pub fn on_window_gone(&mut self, hwnd: isize) -> bool {
        self.on_window_gone_at(hwnd, Instant::now())
    }

    /// [`Self::on_window_gone`] at `now`: the process stays adoptable for
    /// [`ADOPT_FOR`] (its next foreground window is the session).
    pub fn on_window_gone_at(&mut self, hwnd: isize, now: Instant) -> bool {
        if let Some(c) = self.current.take_if(|c| c.hwnd == hwnd) {
            self.adoptable = Some((c.pid, now));
            self.replaced_at = None;
            return true;
        }
        false
    }
}

#[cfg(test)]
#[path = "record_target_tests.rs"]
mod tests;
