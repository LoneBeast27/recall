//! A game session stuck in `starting`: the engine runs and has said hello for this
//! target, but its encoder never gets a first frame (the capture failed to start or died
//! before it). After [`STUCK_AFTER`] the engine gets ONE intended restart. If the first
//! frame still does not come within another [`STUCK_AFTER`], the island says
//! "Can't capture <game>" and the failure cue plays, once. After that nothing respawns
//! until the target changes (another game: another process) or the game's window mode
//! changes (fullscreen <-> windowed, [`WindowMode`]). Shortcuts, a pause / resume and a
//! resize or move change neither, so they never re-arm it (p2 11-07). A new window of the
//! same game (some games recreate it on a mode switch) counts only when its mode differs.
//! A first frame that does come ends the episode, so a later stall is news again. The
//! engine retries its own capture meanwhile (`recall` `capture_watchdog`), on evidence only.
//!
//! Only time the player could see the game counts: while it is minimized or behind
//! another window, the session is locked or the display is off (`record_screen`), a game
//! sends no frames (Desktop Duplication holds them too), so the 10 s clock starts over.
//! It also stands aside while an encoder recovery is pending (`record_recover`), and that
//! one while this is, so the two never stack engine restarts.
//!
//! Nothing is lost by the restart: an encoder still "starting" has no frames.

use std::time::{Duration, Instant};

use crate::log;
use crate::record_app::Controller;
use crate::record_engine::EngineTarget;
use crate::record_notice::shortcut_notice_json;
use crate::record_screen::{Sight, WindowMode};
use crate::record_status::session_state;
use crate::record_target::Target;
use crate::supervisor::EngineHealth;

/// `starting` this long with a fresh, running engine is stuck.
pub const STUCK_AFTER: Duration = Duration::from_secs(10);
/// How long the one restart counts as in flight for the encoder recovery (which stands
/// aside meanwhile). Bounded: an engine that comes back in `error`, never `starting`,
/// would otherwise hold both watches off for good (review 11-07).
pub const RESTART_GRACE: Duration = Duration::from_secs(20);

/// What to do for a stuck session (one of the two).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StuckAct {
    /// Restart the engine (the first time for this target).
    pub restart: bool,
    /// The island notice to send (the second time; dropped when the overlay is off).
    pub notice: Option<String>,
}

/// How far this target's episode has gone.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum Phase {
    #[default]
    Fresh,
    /// The one engine restart was made.
    Restarted,
    /// The notice was given: hands off until something changes.
    Told,
}

/// Follows (state, target) each tick.
#[derive(Debug, Default)]
pub struct StuckWatch {
    /// The game (its pid) and since when it has been stuck-shaped.
    since: Option<(u32, Instant)>,
    /// The game the episode belongs to, and its phase.
    phase: Option<(u32, Phase)>,
    /// When the one restart was made (bounds `pending`).
    restarted_at: Option<Instant>,
    /// The game and its window mode last seen (a change of mode starts a new episode).
    mode: Option<(u32, WindowMode)>,
}

impl StuckWatch {
    /// `state` is `record_status::session_state`; `running` / `fresh` as there; `shown`:
    /// the game can be sending frames now (`Sight::shown`).
    pub fn observe(&mut self, state: &str, wanted: &EngineTarget, running: bool, fresh: bool, shown: bool, now: Instant) -> Option<StuckAct> {
        let EngineTarget::Window(t) = wanted else {
            self.since = None;
            return None;
        };
        let key = t.pid;
        if self.phase.is_some_and(|(k, _)| k != key) || state == "recording" {
            self.phase = None; // a new target, or this one recovered: a later stall is news again
        }
        if state != "starting" || !running || !fresh || !shown {
            self.since = None;
            return None;
        }
        let since = match self.since {
            Some((k, at)) if k == key => at,
            _ => {
                self.since = Some((key, now));
                now
            }
        };
        if now.duration_since(since) < STUCK_AFTER {
            return None;
        }
        self.since = None;
        let (phase, act) = match self.phase.map_or(Phase::Fresh, |(_, p)| p) {
            Phase::Fresh => (Phase::Restarted, StuckAct { restart: true, notice: None }),
            Phase::Restarted => {
                let notice = shortcut_notice_json(&format!("Can't capture {}", t.name), "Try another window mode", true, false);
                (Phase::Told, StuckAct { restart: false, notice: Some(notice) })
            }
            Phase::Told => return None,
        };
        if phase == Phase::Restarted {
            self.restarted_at = Some(now);
        }
        self.phase = Some((key, phase));
        Some(act)
    }

    /// The clock runs, or the restart was made under [`RESTART_GRACE`] ago and its first
    /// frame is still awaited.
    pub fn pending(&self, now: Instant) -> bool {
        let in_flight = self.restarted_at.is_some_and(|at| now.duration_since(at) < RESTART_GRACE);
        self.since.is_some() || (in_flight && self.phase.is_some_and(|(_, p)| p == Phase::Restarted))
    }

    /// The game's window mode now: a change (same game) starts a new episode.
    fn note_mode(&mut self, seen: Option<(u32, WindowMode)>) {
        let Some((pid, mode)) = seen else { return };
        if self.mode.is_some_and(|(p, m)| p == pid && m != mode) {
            self.phase = None;
            self.since = None; // the new episode's 10 s start now
        }
        self.mode = seen;
    }
}

/// What `capture_stuck_tick` reads from the controller each tick.
#[derive(Debug, Clone)]
pub struct StuckInputs<'a> {
    pub wanted: &'a EngineTarget,
    pub health: EngineHealth,
    pub encoder: Option<&'a str>,
    /// A hello from the engine started for this target.
    pub fresh: bool,
    pub paused: bool,
    pub running: bool,
    /// A drain runs, the engine is busy (a save, a long recording) or an encoder
    /// recovery is pending: hands off.
    pub held: bool,
    pub overlay: bool,
}

/// One tick: the act to apply (its notice already dropped when the overlay is off), with
/// `sight` asked only for a game target.
pub fn stuck_step(w: &mut StuckWatch, i: &StuckInputs, sight: impl Fn(&Target) -> Sight, now: Instant) -> Option<StuckAct> {
    if i.held {
        return None;
    }
    let state = session_state(i.wanted, i.health, i.encoder, i.fresh, i.paused);
    let seen = match i.wanted {
        EngineTarget::Window(t) if state == "starting" => Some((t.pid, sight(t))),
        _ => None,
    };
    w.note_mode(seen.map(|(pid, s)| (pid, s.mode())));
    let seen = seen.map(|(_, s)| s);
    let act = w.observe(state, i.wanted, i.running, i.fresh, seen.is_none_or(Sight::shown), now)?;
    Some(StuckAct { notice: act.notice.filter(|_| i.overlay), ..act })
}

impl Controller {
    /// Pump tick: a stuck game session gets one intended restart, then one notice.
    pub(crate) fn capture_stuck_tick(&mut self, now: Instant) {
        let wanted = self.wanted();
        let encoder = self.supervisor.engine_view().encoder;
        let inputs = StuckInputs {
            wanted: &wanted,
            health: self.supervisor.health(),
            encoder: encoder.as_deref(),
            fresh: self.supervisor.hello_count() > self.hello_at_retarget,
            paused: self.paused,
            running: self.engine_running(),
            held: self.planner.drain().is_some() || self.activity.busy(now) || self.recovery.pending(now),
            overlay: self.cfg.overlay.enabled,
        };
        let Some(act) = stuck_step(&mut self.capture_stuck, &inputs, crate::record_screen::sight, now) else { return };
        let target = crate::record_status::target_json(&wanted);
        if act.restart {
            log!("record: no first frame after {} s for {target:?}; restarting the engine once", STUCK_AFTER.as_secs());
            self.restart_engine(crate::record_restart::RestartReason::Recovery);
        } else {
            log!("record: still no frame for {target:?} after the restart; told the player, no more restarts until something changes");
        }
        if let Some(notice) = act.notice {
            self.hub.event("session", &notice);
            crate::cues::failed();
        }
    }
}

#[cfg(test)]
#[path = "record_capture_watch_tests.rs"]
mod tests;
