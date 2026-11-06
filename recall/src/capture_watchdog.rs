//! Capture watchdog: a capture that failed to start, or a WGC capture whose thread
//! ended (the handler returned an error, e.g. a frame read failed), is started again
//! from `periodic_update` with a backoff of 1, 2, 5, 10 s, then every 10 s. Each try
//! and each death is logged. The decision is [`Backoff`], a pure helper; the
//! [`App`] glue below only reads the handle and calls `start_capture`.

use std::time::{Duration, Instant};

use crate::App;

/// Waits between tries, by the number of failures so far (the last one repeats).
const DELAYS: [Duration; 4] =
    [Duration::from_secs(1), Duration::from_secs(2), Duration::from_secs(5), Duration::from_secs(10)];

/// A capture that ran this long before it died was healthy: the backoff starts over.
const STABLE: Duration = Duration::from_secs(30);

/// What the watchdog wants done this tick.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Step {
    Idle,
    /// No capture and the wait is over: start one.
    Start,
    /// The capture thread has finished: stop (join) the handle, then wait.
    Reap,
}

/// Failure count and the next try time.
#[derive(Debug, Default)]
pub(crate) struct Backoff {
    failures: u32,
    next_try: Option<Instant>,
    started_at: Option<Instant>,
}

impl Backoff {
    /// The wait after `failures` failures (1-based).
    pub(crate) fn delay(failures: u32) -> Duration {
        let i = (failures.max(1) - 1) as usize;
        DELAYS[i.min(DELAYS.len() - 1)]
    }

    pub(crate) fn failures(&self) -> u32 {
        self.failures
    }

    /// A start succeeded. The failure count stays until the capture proves stable,
    /// so a capture that dies right after each start keeps backing off.
    pub(crate) fn started(&mut self, now: Instant) {
        self.next_try = None;
        self.started_at = Some(now);
    }

    /// A start failed; returns the wait before the next try.
    pub(crate) fn start_failed(&mut self, now: Instant) -> Duration {
        self.started_at = None;
        self.schedule(now)
    }

    /// The running capture's thread ended; returns the wait before the restart.
    pub(crate) fn died(&mut self, now: Instant) -> Duration {
        if self.started_at.is_some_and(|t| now.duration_since(t) >= STABLE) {
            self.failures = 0;
        }
        self.started_at = None;
        self.schedule(now)
    }

    fn schedule(&mut self, now: Instant) -> Duration {
        self.failures = self.failures.saturating_add(1);
        let wait = Self::delay(self.failures);
        self.next_try = Some(now + wait);
        wait
    }

    /// `has_capture`: the app holds a handle; `finished`: that handle's thread ended.
    pub(crate) fn step(&self, now: Instant, has_capture: bool, finished: bool) -> Step {
        match (has_capture, finished) {
            (true, true) => Step::Reap,
            (true, false) => Step::Idle,
            (false, _) if self.next_try.is_some_and(|t| now >= t) => Step::Start,
            (false, _) => Step::Idle,
        }
    }
}

impl App {
    /// Called from `periodic_update`: retry a failed start, or restart a dead capture.
    pub(super) fn watch_capture(&mut self) {
        if self.shared.is_none() {
            return;
        }
        let now = Instant::now();
        let finished = self.capture.as_ref().is_some_and(|c| c.is_finished());
        match self.capture_watch.step(now, self.capture.is_some(), finished) {
            Step::Idle => {}
            Step::Start => {
                eprintln!("[recall] capture: retrying the start (try {})", self.capture_watch.failures() + 1);
                self.start_capture();
            }
            Step::Reap => {
                let why = match self.capture.take().map(|c| c.stop()) {
                    Some(Err(e)) => e,
                    _ => "the capture thread ended".into(),
                };
                let wait = self.capture_watch.died(now);
                eprintln!("[recall] capture stopped ({why}); restarting in {} s", wait.as_secs());
            }
        }
    }

    /// `start_capture`'s result for the watchdog (logged with the next try).
    pub(super) fn note_capture_start(&mut self, result: Result<(), String>) {
        let now = Instant::now();
        match result {
            Ok(()) => self.capture_watch.started(now),
            Err(e) => {
                let wait = self.capture_watch.start_failed(now);
                eprintln!("[recall] failed to start capture: {e}; retrying in {} s", wait.as_secs());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn secs(s: u64) -> Duration {
        Duration::from_secs(s)
    }

    #[test]
    fn a_failed_start_is_retried_after_1_2_5_10_then_every_10_seconds() {
        let t0 = Instant::now();
        let mut b = Backoff::default();
        let mut now = t0;
        let mut waits = Vec::new();
        for _ in 0..6 {
            let wait = b.start_failed(now);
            waits.push(wait.as_secs());
            assert_eq!(b.step(now + wait - Duration::from_millis(1), false, false), Step::Idle, "not before the wait");
            assert_eq!(b.step(now + wait, false, false), Step::Start);
            now += wait;
        }
        assert_eq!(waits, [1, 2, 5, 10, 10, 10]);
    }

    #[test]
    fn nothing_is_tried_before_any_start_or_while_a_capture_runs() {
        let t0 = Instant::now();
        let mut b = Backoff::default();
        assert_eq!(b.step(t0 + secs(100), false, false), Step::Idle, "never started: start_capture's own call");
        b.started(t0);
        assert_eq!(b.step(t0 + secs(100), true, false), Step::Idle);
    }

    #[test]
    fn a_dead_capture_thread_is_reaped_and_restarted_with_backoff() {
        let t0 = Instant::now();
        let mut b = Backoff::default();
        b.started(t0);
        assert_eq!(b.step(t0 + secs(3), true, true), Step::Reap);
        assert_eq!(b.died(t0 + secs(3)), secs(1));
        assert_eq!(b.step(t0 + secs(4), false, false), Step::Start);
        // Dies again right after the restart: the wait grows.
        b.started(t0 + secs(4));
        assert_eq!(b.died(t0 + secs(5)), secs(2));
    }

    #[test]
    fn a_capture_that_ran_stable_starts_the_backoff_over() {
        let t0 = Instant::now();
        let mut b = Backoff::default();
        for i in 0..4 {
            b.start_failed(t0 + secs(i));
        }
        b.started(t0 + secs(10));
        assert_eq!(b.died(t0 + secs(10) + STABLE), secs(1));
    }
}
