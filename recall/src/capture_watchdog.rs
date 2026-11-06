//! Capture watchdog: a capture that failed to start, or a WGC capture whose thread
//! ended (the handler returned an error, e.g. a frame read failed), is started again
//! from `periodic_update` with a backoff of 1, 2, 5, 10 s, then every 10 s. Each try
//! and each death is logged. The decision is [`Backoff`], a pure helper; [`watch`] runs it
//! over a [`CaptureSlot`] (the [`App`], or a fake in the tests).
//!
//! While the target is minimized, the session is locked or the display is off
//! (`capture::screen`) nothing is reaped or started: a start then fails or gets no
//! frames, and each restart opens a new D3D11 device and restarts the replay buffer.
//! The wait resumes when the user is back.

use std::time::{Duration, Instant};

use crate::capture::screen::{self, Screen};
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

/// What the watchdog reads and drives.
pub(crate) trait CaptureSlot {
    fn backoff(&mut self) -> &mut Backoff;
    /// A capture handle is held, and whether its thread has ended.
    fn held(&self) -> (bool, bool);
    /// Stops (joins) the held capture; its error, if any.
    fn stop(&mut self) -> Option<String>;
    /// Starts a capture (not yet reported to the backoff).
    fn try_start(&mut self) -> Result<(), String>;
    fn screen(&self) -> Screen;
}

/// One watchdog tick; returns what it did.
pub(crate) fn watch(slot: &mut impl CaptureSlot, now: Instant) -> Step {
    let (has_capture, finished) = slot.held();
    let step = slot.backoff().step(now, has_capture, finished);
    if step == Step::Idle || slot.screen().away() {
        return Step::Idle;
    }
    match step {
        Step::Start => {
            eprintln!("[recall] capture: retrying the start (try {})", slot.backoff().failures() + 1);
            let result = slot.try_start();
            note_start(slot.backoff(), result, now);
        }
        _ => {
            let why = slot.stop().unwrap_or_else(|| "the capture thread ended".into());
            let wait = slot.backoff().died(now);
            eprintln!("[recall] capture stopped ({why}); restarting in {} s", wait.as_secs());
        }
    }
    step
}

/// A start's result for the backoff (a failure is logged with the next try).
pub(crate) fn note_start(b: &mut Backoff, result: Result<(), String>, now: Instant) {
    match result {
        Ok(()) => b.started(now),
        Err(e) => {
            let wait = b.start_failed(now);
            eprintln!("[recall] failed to start capture: {e}; retrying in {} s", wait.as_secs());
        }
    }
}

impl CaptureSlot for App {
    fn backoff(&mut self) -> &mut Backoff {
        &mut self.capture_watch
    }

    fn held(&self) -> (bool, bool) {
        (self.capture.is_some(), self.capture.as_ref().is_some_and(|c| c.is_finished()))
    }

    fn stop(&mut self) -> Option<String> {
        self.capture.take().and_then(|c| c.stop().err())
    }

    fn try_start(&mut self) -> Result<(), String> {
        self.try_start_capture()
    }

    fn screen(&self) -> Screen {
        screen::now(std::env::var("RECALL_CAPTURE_HWND").ok().and_then(|v| v.parse::<usize>().ok()))
    }
}

impl App {
    /// Called from `periodic_update`: retry a failed start, or restart a dead capture.
    pub(super) fn watch_capture(&mut self) {
        if self.shared.is_some() {
            watch(self, Instant::now());
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

    // `watch` (App::watch_capture's body) over a fake slot.

    /// A capture slot whose starts succeed or fail as told, and whose screen is `screen`.
    #[derive(Default)]
    struct Fake {
        backoff: Backoff,
        capture: bool,
        finished: bool,
        start_ok: bool,
        screen: Screen,
        starts: u32,
        stops: u32,
    }

    impl CaptureSlot for Fake {
        fn backoff(&mut self) -> &mut Backoff {
            &mut self.backoff
        }
        fn held(&self) -> (bool, bool) {
            (self.capture, self.capture && self.finished)
        }
        fn stop(&mut self) -> Option<String> {
            self.stops += 1;
            self.capture = false;
            self.finished = false;
            None
        }
        fn try_start(&mut self) -> Result<(), String> {
            self.starts += 1;
            self.capture = self.start_ok;
            if self.start_ok { Ok(()) } else { Err("no frames source".into()) }
        }
        fn screen(&self) -> Screen {
            self.screen
        }
    }

    const MINIMIZED: Screen = Screen { iconic: true, locked: false, display_off: false };
    const LOCKED: Screen = Screen { iconic: false, locked: true, display_off: false };
    const DISPLAY_OFF: Screen = Screen { iconic: false, locked: false, display_off: true };

    /// A capture whose thread ended at t0, then `screen` for seconds 0..=to.
    fn died_then(screen: Screen, to: u64) -> (Fake, Instant) {
        let t0 = Instant::now();
        let mut f = Fake { capture: true, finished: true, start_ok: true, screen, ..Default::default() };
        f.backoff.started(t0 - secs(60));
        for s in 0..=to {
            watch(&mut f, t0 + secs(s));
        }
        (f, t0)
    }

    #[test]
    fn a_dead_capture_is_not_reaped_or_restarted_while_minimized_locked_or_the_display_is_off() {
        for (why, screen) in [("minimized", MINIMIZED), ("locked", LOCKED), ("display off", DISPLAY_OFF)] {
            let (f, _) = died_then(screen, 60);
            assert_eq!((f.stops, f.starts), (0, 0), "{why}: 60 s");
        }
    }

    #[test]
    fn a_dead_capture_on_screen_is_reaped_and_restarted_once() {
        let (f, _) = died_then(Screen::default(), 60);
        assert_eq!((f.stops, f.starts), (1, 1));
        assert!(f.capture);
    }

    #[test]
    fn a_failed_start_waits_while_locked_and_retries_once_the_user_is_back() {
        let t0 = Instant::now();
        let mut f = Fake { screen: LOCKED, ..Default::default() };
        note_start(&mut f.backoff, Err("lock screen".into()), t0);
        for s in 0..=60 {
            watch(&mut f, t0 + secs(s));
        }
        assert_eq!(f.starts, 0, "locked: no tries");
        f.screen = Screen::default();
        f.start_ok = true;
        assert_eq!(watch(&mut f, t0 + secs(61)), Step::Start);
        assert_eq!(f.starts, 1);
        assert_eq!(watch(&mut f, t0 + secs(62)), Step::Idle, "running: left alone");
    }
}
