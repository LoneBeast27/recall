//! A released engine's wait in `finish_shutdown` (design-detect-release P10-P12, P19). The
//! engine judges its own saves (it gives a stalled one up and says so); the supervisor only
//! kills an engine that is hung, done, or out of time. Every poll recomputes the kill moment
//! from stored facts ([`ReleaseWait::kill_at`]), all on the awake-only clock: no derived
//! deadline is kept, so a heartbeat that comes back after a silence moves the kill on again
//! (the old `min`-only deadline stuck, R2-p7-1), and a sleep ages nothing (R2-p7-2, B9).
//!
//! `kill_at = min(released + RELEASE_GRACE, settled + QUIT_GRACE, stop + its grace,
//! heartbeats ? max(last beat, released, gap) + HEARTBEAT_LOST + QUIT_GRACE : never)`.
//! `gap` is a poll that came more than [`POLL_GAP`] of awake time after the one before: the
//! supervisor itself did not run, so the silence was not the engine's.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use super::release::RELEASE_GRACE;
use super::{Shared, QUIT_GRACE};
use crate::awake_clock::{Awake, AwakeClock, Clock};

/// No heartbeat for this much awake time: the engine is hung (not its save, which it judges).
pub(super) const HEARTBEAT_LOST: Duration = Duration::from_secs(10);
/// A poll this long (awake) after the previous one: the observer stalled, not the engine.
pub(super) const POLL_GAP: Duration = Duration::from_secs(2);

/// The clock the supervisor reads, and the last drain heartbeat on it.
pub(super) struct Beats {
    pub(super) clock: Arc<dyn Clock>,
    last: Mutex<Option<Awake>>,
}

impl Default for Beats {
    fn default() -> Self {
        Beats { clock: Arc::new(AwakeClock), last: Mutex::new(None) }
    }
}

impl Shared {
    /// A drain heartbeat (`save_progress`) arrived (the stdout reader, `protocol_line`).
    pub(super) fn note_beat(&self) {
        let now = self.beats.clock.awake();
        *self.beats.last.lock().unwrap_or_else(|p| p.into_inner()) = Some(now);
    }

    /// The last heartbeat on the awake clock.
    pub(super) fn last_beat(&self) -> Option<Awake> {
        *self.beats.last.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// Now on the supervisor's awake clock.
    pub(super) fn awake(&self) -> Awake {
        self.beats.clock.awake()
    }

    /// Tests: a supervisor state on `clock`.
    #[cfg(test)]
    pub(super) fn with_clock(clock: Arc<dyn Clock>) -> Shared {
        let mut shared = Shared::new(None);
        shared.beats.clock = clock;
        shared
    }
}

/// The facts of one release's wait. Nothing derived is stored.
#[derive(Debug, Clone, Copy)]
pub(super) struct ReleaseWait {
    released_at: Awake,
    /// The engine's hello says `drain_heartbeat`: its silence means it is hung.
    beats: bool,
    /// The last poll that came after an observer gap.
    gap_at: Option<Awake>,
    /// The first poll that saw the controller settle the release (all saves answered).
    settled_at: Option<Awake>,
    /// The first poll that saw the supervisor stop (app quit), and its grace.
    stop: Option<(Awake, Duration)>,
    last_poll: Awake,
}

impl ReleaseWait {
    pub(super) fn new(now: Awake, beats: bool) -> Self {
        ReleaseWait { released_at: now, beats, gap_at: None, settled_at: None, stop: None, last_poll: now }
    }

    /// One poll at `now`: records the facts, then whether to kill the engine now.
    pub(super) fn poll(&mut self, now: Awake, last_beat: Option<Awake>, settled: bool, stopping: Option<Duration>) -> bool {
        if now.saturating_duration_since(self.last_poll) > POLL_GAP {
            self.gap_at = Some(now);
        }
        self.last_poll = now;
        if settled {
            self.settled_at.get_or_insert(now);
        }
        if let Some(grace) = stopping {
            self.stop.get_or_insert((now, grace));
        }
        now >= self.kill_at(last_beat)
    }

    /// When the engine is killed, from the facts and the last heartbeat (see the module notes).
    pub(super) fn kill_at(&self, last_beat: Option<Awake>) -> Awake {
        let mut at = self.released_at + RELEASE_GRACE;
        if let Some(settled) = self.settled_at {
            at = at.min(settled + QUIT_GRACE);
        }
        if let Some((stop, grace)) = self.stop {
            at = at.min(stop + grace);
        }
        if self.beats {
            let alive = [last_beat, self.gap_at].into_iter().flatten().fold(self.released_at, Awake::max);
            at = at.min(alive + HEARTBEAT_LOST + QUIT_GRACE);
        }
        at
    }
}

#[cfg(test)]
#[path = "supervisor_release_wait_tests.rs"]
mod tests;
