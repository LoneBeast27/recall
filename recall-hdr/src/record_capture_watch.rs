//! A game session stuck in `starting`: the engine runs and has said hello for this
//! target, but its encoder never gets a frame (the capture failed to start or died
//! before the first frame). After [`STUCK_AFTER`] the island says "Can't capture
//! <game>" / "Retrying" once and the engine gets one intended restart; later
//! restarts for the same target wait [`RETRY_EVERY`] since the last one. The
//! engine retries its own capture meanwhile (`recall` `capture_watchdog`).
//!
//! Nothing is lost by the restart: an encoder still "starting" has no frames.

use std::time::{Duration, Instant};

use crate::log;
use crate::record_app::Controller;
use crate::record_engine::EngineTarget;
use crate::record_notice::shortcut_notice_json;
use crate::record_status::session_state;

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
    /// `state` is `record_status::session_state`; `running` / `fresh` as there.
    pub fn observe(&mut self, state: &str, wanted: &EngineTarget, running: bool, fresh: bool, now: Instant) -> Option<StuckAct> {
        let EngineTarget::Window(t) = wanted else {
            self.since = None;
            return None;
        };
        let key = (t.hwnd, t.pid);
        if self.acted.is_some_and(|(k, _)| k != key) || state == "recording" {
            self.acted = None; // a new target, or this one recovered: a later stall is news again
        }
        if state != "starting" || !running || !fresh {
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

impl Controller {
    /// Pump tick: a stuck game session gets its notice and an intended restart.
    pub(crate) fn capture_stuck_tick(&mut self, now: Instant) {
        if self.planner.drain().is_some() || self.activity.busy(now) {
            return;
        }
        let wanted = self.wanted();
        let health = self.supervisor.health();
        let encoder = self.supervisor.engine_view().encoder;
        let fresh = self.supervisor.hello_count() > self.hello_at_retarget;
        let state = session_state(&wanted, health, encoder.as_deref(), fresh, self.paused);
        let Some(act) = self.capture_stuck.observe(state, &wanted, self.engine_running(), fresh, now) else { return };
        log!("record: no frames after {} s for {:?}; restarting the engine", STUCK_AFTER.as_secs(), crate::record_status::target_json(&wanted));
        if let Some(notice) = act.notice.filter(|_| self.cfg.overlay.enabled) {
            self.hub.event("session", &notice);
            crate::cues::failed();
        }
        self.restart_engine_for_settings();
    }
}

#[cfg(test)]
#[path = "record_capture_watch_tests.rs"]
mod tests;
