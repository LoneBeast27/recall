//! A game session stuck in `starting`: the engine runs and has said hello for this
//! target, but its encoder never gets a frame (the capture failed to start or died
//! before the first frame). After [`STUCK_AFTER`] the island says "Can't capture
//! <game>" / "Retrying" once and the engine gets one intended restart; later
//! restarts for the same target wait [`RETRY_EVERY`] since the last one. The
//! engine retries its own capture meanwhile (`recall` `capture_watchdog`).
//!
//! Only time the player could see the game counts: while it is minimized or behind
//! another window, the session is locked or the display is off (`record_screen`), a game
//! sends no frames (Desktop Duplication holds them too), so the 10 s clock starts over.
//!
//! Nothing is lost by the restart: an encoder still "starting" has no frames.

use std::time::{Duration, Instant};

use crate::log;
use crate::record_app::Controller;
use crate::record_engine::EngineTarget;
use crate::record_notice::shortcut_notice_json;
use crate::record_screen::Sight;
use crate::record_status::session_state;
use crate::record_target::Target;
use crate::supervisor::EngineHealth;

/// `starting` this long with a fresh, running engine is stuck.
pub const STUCK_AFTER: Duration = Duration::from_secs(10);
/// At most one restart per this long for the same target.
pub const RETRY_EVERY: Duration = Duration::from_secs(60);

/// What to do for a stuck session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StuckAct {
    /// The island notice to send (the first restart for this target only).
    pub notice: Option<String>,
}

/// Follows (state, target) each tick.
#[derive(Debug, Default)]
pub struct StuckWatch {
    /// The target (hwnd, pid) and since when it has been stuck-shaped.
    since: Option<((isize, u32), Instant)>,
    /// The target the notice / last restart belong to, and that restart's time.
    acted: Option<((isize, u32), Instant)>,
}

impl StuckWatch {
    /// `state` is `record_status::session_state`; `running` / `fresh` as there; `shown`:
    /// the game can be sending frames now (`Sight::shown`).
    pub fn observe(&mut self, state: &str, wanted: &EngineTarget, running: bool, fresh: bool, shown: bool, now: Instant) -> Option<StuckAct> {
        let EngineTarget::Window(t) = wanted else {
            self.since = None;
            return None;
        };
        let key = (t.hwnd, t.pid);
        if self.acted.is_some_and(|(k, _)| k != key) || state == "recording" {
            self.acted = None; // a new target, or this one recovered: a later stall is news again
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
        let notice = match self.acted {
            Some((_, last)) if now.duration_since(last) < RETRY_EVERY => return None,
            Some(_) => None,
            None => Some(shortcut_notice_json(&format!("Can't capture {}", t.name), "Retrying", true, false)),
        };
        self.acted = Some((key, now));
        self.since = None;
        Some(StuckAct { notice })
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
    /// A drain runs or the engine is busy (a save, a long recording): hands off.
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
    let shown = match i.wanted {
        EngineTarget::Window(t) if state == "starting" => sight(t).shown(),
        _ => true,
    };
    let act = w.observe(state, i.wanted, i.running, i.fresh, shown, now)?;
    Some(StuckAct { notice: act.notice.filter(|_| i.overlay) })
}

impl Controller {
    /// Pump tick: a stuck game session gets its notice and an intended restart.
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
            held: self.planner.drain().is_some() || self.activity.busy(now),
            overlay: self.cfg.overlay.enabled,
        };
        let Some(act) = stuck_step(&mut self.capture_stuck, &inputs, crate::record_screen::sight, now) else { return };
        log!("record: no frames after {} s for {:?}; restarting the engine", STUCK_AFTER.as_secs(), crate::record_status::target_json(&wanted));
        if let Some(notice) = act.notice {
            self.hub.event("session", &notice);
            crate::cues::failed();
        }
        self.restart_engine_for_settings();
    }
}

#[cfg(test)]
#[path = "record_capture_watch_tests.rs"]
mod tests;
