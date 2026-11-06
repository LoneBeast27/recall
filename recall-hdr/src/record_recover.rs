//! Engine restart after a lasting encoder failure (the controller's half of encoder
//! recovery; the engine first retries the open itself, `recall/src/encoder/reopen.rs`).
//!
//! When the session sits in `error` from `encoder: failed` for longer than
//! [`ERROR_GRACE`], the engine is restarted as an INTENDED restart (a retarget to the same
//! target: the supervisor never counts it toward give-up). These restarts have their own
//! backoff ([`RESTART_GAPS`]); a minute of recording forgets it. The island says
//! "Recording again" when recording comes back (`record_notice`).

use std::time::{Duration, Instant};

use crate::log;
use crate::record_status::session_state;
use crate::supervisor::EngineHealth;

/// The session must sit in `error` from `encoder: failed` longer than this.
pub const ERROR_GRACE: Duration = Duration::from_secs(15);
/// Least time between two of these restarts: 30 s, 60 s, 2 min, then every 5 min.
pub const RESTART_GAPS: [Duration; 4] =
    [Duration::from_secs(30), Duration::from_secs(60), Duration::from_secs(120), Duration::from_secs(300)];
/// Recording this long after a restart forgets the backoff.
pub const STABLE_AFTER: Duration = Duration::from_secs(60);

/// The session as this rule sees it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Seen {
    /// `error` because the engine reported `encoder: failed` (not a supervisor give-up).
    EncoderFailed,
    Recording,
    /// Engine starting or restarting.
    Starting,
    /// Waiting, paused, draining or given up: nothing to recover.
    Idle,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Recovery {
    Nothing,
    /// Restart the engine now (an intended restart).
    Restart,
    /// Recording came back after a restart.
    Recovered,
}

impl Seen {
    /// From a `record_status::session_state` and the supervisor's health.
    pub fn of(state: &str, health: EngineHealth) -> Seen {
        match state {
            "error" if health != EngineHealth::GivenUp => Seen::EncoderFailed,
            "recording" => Seen::Recording,
            "starting" => Seen::Starting,
            _ => Seen::Idle,
        }
    }
}

#[derive(Debug, Default)]
pub struct EncoderRecovery {
    /// Since when the error has lasted without a break.
    failed_since: Option<Instant>,
    /// Restarts in this episode and when the last one was asked.
    restarts: u32,
    last_restart: Option<Instant>,
    /// A restart was asked and recording has not come back yet.
    awaiting: bool,
    /// Recording since (after a restart): a stable minute ends the episode.
    healthy_since: Option<Instant>,
}

impl EncoderRecovery {
    /// Feed the session every pump tick; `busy` = a save or the long recording in flight.
    pub fn observe(&mut self, now: Instant, seen: Seen, busy: bool) -> Recovery {
        if seen != Seen::EncoderFailed {
            self.failed_since = None;
        }
        match seen {
            Seen::EncoderFailed => {
                self.healthy_since = None;
                let since = *self.failed_since.get_or_insert(now);
                if now.saturating_duration_since(since) <= ERROR_GRACE || busy || !self.gap_passed(now) {
                    return Recovery::Nothing;
                }
                self.restarts = self.restarts.saturating_add(1);
                self.last_restart = Some(now);
                self.failed_since = None;
                self.awaiting = true;
                Recovery::Restart
            }
            Seen::Recording => self.recording(now),
            Seen::Starting => Recovery::Nothing,
            Seen::Idle => {
                *self = EncoderRecovery::default();
                Recovery::Nothing
            }
        }
    }

    fn recording(&mut self, now: Instant) -> Recovery {
        if self.restarts == 0 {
            return Recovery::Nothing;
        }
        let since = *self.healthy_since.get_or_insert(now);
        if now.saturating_duration_since(since) >= STABLE_AFTER {
            *self = EncoderRecovery::default();
            return Recovery::Nothing;
        }
        if std::mem::take(&mut self.awaiting) {
            return Recovery::Recovered;
        }
        Recovery::Nothing
    }

    /// The backoff since the last restart has run out (no restart yet: it has).
    fn gap_passed(&self, now: Instant) -> bool {
        let gap = RESTART_GAPS[(self.restarts.max(1) as usize - 1).min(RESTART_GAPS.len() - 1)];
        self.last_restart.is_none_or(|at| now.saturating_duration_since(at) >= gap)
    }
}

impl crate::record_app::Controller {
    /// Every pump tick: restart the engine when the encoder error has lasted (intended).
    pub(crate) fn encoder_recover_tick(&mut self, now: Instant) {
        let health = self.supervisor.health();
        let encoder = self.supervisor.engine_view().encoder;
        let fresh = self.supervisor.hello_count() > self.hello_at_retarget;
        let state = session_state(&self.wanted(), health, encoder.as_deref(), fresh, self.paused);
        let busy = self.activity.busy(now);
        match self.recovery.observe(now, Seen::of(state, health), busy) {
            Recovery::Restart => {
                log!("record: the encoder has failed for over {} s; restarting the engine (intended)", ERROR_GRACE.as_secs());
                self.restart_engine_for_settings();
            }
            Recovery::Recovered => log!("record: recording again after the engine restart"),
            Recovery::Nothing => {}
        }
    }
}

#[cfg(test)]
#[path = "record_recover_tests.rs"]
mod tests;
