//! Engine restart after a lasting encoder failure: the last resort of encoder recovery.
//!
//! The engine goes first (`recall/src/encoder/reopen.rs`): it rebuilds the encoder in
//! place keeping the replay buffer, asks for a capture restart, falls back to CPU copies,
//! and meanwhile reports `encoder: recovering` (session `recovering`). It reports
//! `failed` only once that plan is spent (5 failed reopens or 60 s without an encoder).
//! The controller is keyed off that engine state, not off a shorter timer.
//!
//! When the session sits in `error` from `encoder: failed` for longer than
//! [`ERROR_GRACE`], the engine is restarted as an INTENDED restart (a retarget to the same
//! target: the supervisor never counts it toward give-up). These restarts have their own
//! backoff ([`RESTART_GAPS`]); a minute of recording forgets it. The island says
//! "Recording again" when recording comes back (`record_notice`).
//!
//! A "recovering" engine whose recovery stopped moving counts as `encoder: failed` here
//! (`record_recover_stale`), but only while the game can be seen.

use std::time::{Duration, Instant};

use crate::log;
use crate::record_recover_stale::StaleRecovering;
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
            // CPU copies still record: nothing to restart.
            "recording" | "degraded" => Seen::Recording,
            // The engine's own recovery (encoder rebuild, capture restart, CPU copies) goes
            // first; it says "failed" when that is spent. The restart here is the last resort.
            "starting" | "recovering" => Seen::Starting,
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
    /// A "recovering" that stopped moving.
    stale: StaleRecovering,
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

    /// An error is being timed, or a restart from here was asked under a minute ago and
    /// recording has not come back (`record_capture_watch` stands aside meanwhile).
    pub fn pending(&self, now: Instant) -> bool {
        self.failed_since.is_some() || (self.awaiting && self.last_restart.is_some_and(|t| now.saturating_duration_since(t) < STABLE_AFTER))
    }

    /// One pump tick from the session: `statuses` = status lines so far, `shown` = the game
    /// can be seen. A stale "recovering" is fed as [`Seen::EncoderFailed`].
    pub fn step(&mut self, now: Instant, state: &str, health: EngineHealth, statuses: u64, shown: bool, busy: bool) -> Recovery {
        let seen = if self.stale.observe(state, statuses, shown, now) { Seen::EncoderFailed } else { Seen::of(state, health) };
        self.observe(now, seen, busy)
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
        let view = self.supervisor.engine_view();
        let fresh = self.supervisor.hello_count() > self.hello_at_retarget;
        let wanted = self.wanted();
        let state = session_state(&wanted, health, view.encoder.as_deref(), fresh, self.paused);
        let shown = match &wanted {
            crate::record_engine::EngineTarget::Window(t) if state == "recovering" => crate::record_screen::sight(t).shown(),
            _ => true,
        };
        // A capture-stuck restart in flight: never stack a second one on it.
        let busy = self.activity.busy(now) || self.capture_stuck.pending(now);
        match self.recovery.step(now, state, health, view.statuses, shown, busy) {
            Recovery::Restart => {
                log!("record: the encoder has failed (or its recovery stopped) for over {} s; restarting the engine (intended)", ERROR_GRACE.as_secs());
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
