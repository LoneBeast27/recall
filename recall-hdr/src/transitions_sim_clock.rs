//! The Sim's clocks (design-detect-release §0): `now` is the ordinary clock, which a sleep
//! moves; the activity's timers read the awake-only clock, which only awake time moves
//! (`awake_clock`). And the replay-ring checks the transition tests ask (moved for size).

use std::time::{Duration, Instant};

use super::Sim;
use crate::awake_clock::Awake;
use crate::record_engine::EngineTarget;

impl Sim {
    /// Now on the awake-only clock (what `Controller::awake` reads).
    pub fn awake(&self) -> Awake {
        self.awake_at(self.now)
    }

    /// `t` (on or after the last sleep) on the awake-only clock.
    pub fn awake_at(&self, t: Instant) -> Awake {
        Awake::ZERO + t.saturating_duration_since(self.t0).saturating_sub(self.slept)
    }

    /// The PC sleeps for `d`: `now` moves on, the awake clock does not. No tick runs.
    pub fn sleep_for(&mut self, d: Duration) {
        self.now += d;
        self.slept += d;
    }

    /// Engine spawns (not stops) since `since`.
    pub fn spawns_since(&self, since: Instant) -> usize {
        self.applied.iter().filter(|(at, t)| *at > since && *t != EngineTarget::Off).count()
    }

    /// No engine (re)spawn or stop since `since`: the replay ring is the same one, and a save
    /// in flight was not cut.
    pub fn ring_kept_since(&self, since: Instant) -> bool {
        self.applied.iter().all(|(at, _)| *at <= since)
    }

    /// The ring stopped capturing since `since`: a stop or respawn, or a release.
    pub fn ring_dropped_since(&self, since: Instant) -> bool {
        !self.ring_kept_since(since) || self.released.iter().any(|(at, _)| *at > since)
    }
}
