//! The time the PC sat locked (Win+L) during a long recording (audit U8). The recording
//! keeps going while locked (mic, game audio and a frozen frame); the controller says when
//! the session locks and unlocks (`session_locked`), and the final `recording_stopped`
//! carries the total as `locked_seconds`, so the app and the island can say "12:00 locked".
//!
//! Wall time on the writer's clock: a lock that started before the recording counts from
//! the recording's start, one still on at the stop counts up to the stop. Parts of one
//! recording share the total (only a fresh start resets it).

use std::time::{Duration, Instant};

#[derive(Debug, Default)]
pub struct LockClock {
    locked: bool,
    /// Start of the locked span not yet added to `total` (only while locked).
    since: Option<Instant>,
    total: Duration,
}

impl LockClock {
    /// The session locked (`true`) or unlocked at `now`. A repeat changes nothing.
    pub fn set_locked(&mut self, locked: bool, now: Instant) {
        if locked == self.locked {
            return;
        }
        self.locked = locked;
        if locked {
            self.since = Some(now);
        } else if let Some(since) = self.since.take() {
            self.total += now.saturating_duration_since(since);
        }
    }

    /// A fresh recording started at `now` (part 1): its count starts from zero.
    pub fn started(&mut self, now: Instant) {
        self.total = Duration::ZERO;
        self.since = self.locked.then_some(now);
    }

    /// Seconds locked so far, the open span included.
    pub fn seconds(&self, now: Instant) -> f64 {
        let open = self.since.map_or(Duration::ZERO, |s| now.saturating_duration_since(s));
        (self.total + open).as_secs_f64()
    }

    /// The recording ended at `now`: its locked seconds; the count starts over.
    pub fn stopped(&mut self, now: Instant) -> f64 {
        let secs = self.seconds(now);
        self.total = Duration::ZERO;
        self.since = self.locked.then_some(now);
        secs
    }
}

#[cfg(test)]
mod tests {
    use super::LockClock;
    use std::time::{Duration, Instant};

    const MIN: Duration = Duration::from_secs(60);

    #[test]
    fn a_twelve_minute_lock_during_a_recording_is_720_seconds() {
        let t0 = Instant::now();
        let mut clock = LockClock::default();
        clock.started(t0);
        clock.set_locked(true, t0 + MIN);
        clock.set_locked(false, t0 + 13 * MIN);
        let secs = clock.stopped(t0 + 20 * MIN);
        assert!((secs - 720.0).abs() < 0.01, "{secs}");
    }

    #[test]
    fn a_recording_with_no_lock_has_none() {
        let t0 = Instant::now();
        let mut clock = LockClock::default();
        clock.started(t0);
        assert_eq!(clock.stopped(t0 + 20 * MIN), 0.0);
    }

    #[test]
    fn a_lock_before_the_start_counts_from_the_start_and_one_still_on_up_to_the_stop() {
        let t0 = Instant::now();
        let mut clock = LockClock::default();
        clock.set_locked(true, t0);
        clock.started(t0 + 5 * MIN);
        let secs = clock.stopped(t0 + 8 * MIN);
        assert!((secs - 180.0).abs() < 0.01, "{secs}");
    }

    #[test]
    fn repeats_change_nothing_and_the_next_recording_starts_from_zero() {
        let t0 = Instant::now();
        let mut clock = LockClock::default();
        clock.started(t0);
        clock.set_locked(false, t0 + MIN); // an unlock with no lock
        clock.set_locked(true, t0 + 2 * MIN);
        clock.set_locked(true, t0 + 3 * MIN); // a repeated lock keeps the first start
        clock.set_locked(false, t0 + 4 * MIN);
        assert!((clock.stopped(t0 + 5 * MIN) - 120.0).abs() < 0.01);
        clock.started(t0 + 6 * MIN);
        assert_eq!(clock.stopped(t0 + 7 * MIN), 0.0);
    }
}
