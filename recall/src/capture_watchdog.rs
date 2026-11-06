//! Capture watchdog: a capture that failed to start, or a WGC capture whose thread
//! ended (the handler returned an error, e.g. a frame read failed), is started again
//! from `periodic_update` with a backoff of 1, 2, 5, 10 s, then every 10 s. Each try
//! and each death is logged. The decision is [`Backoff`], a pure helper; [`watch`] runs it
//! over a [`CaptureSlot`] (the [`App`], or a fake in the tests).
//!
//! It also restarts a capture the encoder or the capture thread asked to replace
//! ([`restart_if_asked`], also with no capture held), and a game capture whose recording
//! stopped ([`crate::capture_stall`]).
//!
//! While the target is minimized, the session is locked or the display is off
//! (`capture::screen`) nothing is reaped, started or restarted: a start then fails or
//! gets no frames, and each restart opens a new D3D11 device. The wait resumes when the
//! user is back.

use std::time::{Duration, Instant};

use crate::capture::screen::{self, Screen};
use crate::capture_stall::{Stall, STALL_AFTER};
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
    /// The running game capture's recording, for [`watch`]'s stall check.
    pub(crate) stall: Stall,
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

    /// A restart was asked with no capture held: a start is due now, unless a wait runs
    /// (the start it ends uses the new source anyway).
    pub(crate) fn asked(&mut self, now: Instant) {
        self.next_try.get_or_insert(now);
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
    /// Why the capture must be replaced, if it must (taken: `CaptureShared::restart_reason`).
    fn restart_reason(&mut self) -> Option<&'static str>;
    /// The encoded-frame count while the encoder records (`None` while starting,
    /// recovering, failed or without an encoder).
    fn encoded_frames(&self) -> Option<u64>;
}

/// One watchdog tick; returns what it did.
pub(crate) fn watch(slot: &mut impl CaptureSlot, now: Instant) -> Step {
    let (has_capture, finished) = slot.held();
    let screen = slot.screen();
    let step = slot.backoff().step(now, has_capture, finished);
    if screen.away() {
        return Step::Idle;
    }
    match step {
        Step::Idle => {
            let frames = slot.encoded_frames().filter(|_| has_capture);
            if slot.backoff().stall.observe(frames, screen.watched(), now) {
                eprintln!("[recall] capture stalled: no frame encoded for {} s with the game in front; restarting it", STALL_AFTER.as_secs());
                restart(slot, now);
            }
        }
        Step::Start => {
            eprintln!("[recall] capture: retrying the start (try {})", slot.backoff().failures() + 1);
            let result = slot.try_start();
            note_start(slot.backoff(), result, now);
        }
        Step::Reap => {
            let why = slot.stop().unwrap_or_else(|| "the capture thread ended".into());
            let wait = slot.backoff().died(now);
            eprintln!("[recall] capture stopped ({why}); restarting in {} s", wait.as_secs());
        }
    }
    step
}

/// `restart_capture_if_needed`: replace the capture when asked. With no capture held the
/// request is taken too (the next start must not be torn down by it) and a start is due
/// now unless a backoff wait runs. Nothing while the screen is away (the request waits).
pub(crate) fn restart_if_asked(slot: &mut impl CaptureSlot, now: Instant) {
    if slot.screen().away() {
        return;
    }
    let (has_capture, _) = slot.held();
    let Some(why) = slot.restart_reason() else { return };
    if has_capture {
        eprintln!("[recall] capture restarts {why}");
        restart(slot, now);
    } else {
        eprintln!("[recall] capture restart asked {why} with no capture; the next start uses it");
        slot.backoff().asked(now);
    }
}

/// Stop the held capture and start a new one.
fn restart(slot: &mut impl CaptureSlot, now: Instant) {
    if let Some(e) = slot.stop() {
        eprintln!("[recall] capture stop: {e}");
    }
    let result = slot.try_start();
    note_start(slot.backoff(), result, now);
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

    fn restart_reason(&mut self) -> Option<&'static str> {
        self.shared.as_ref().and_then(|s| s.restart_reason())
    }

    #[cfg(feature = "encode")]
    fn encoded_frames(&self) -> Option<u64> {
        let stats = &self.encoder.as_ref()?.stats;
        matches!(stats.label(), "h264_nvenc" | "degraded").then(|| stats.encoded_frames.load(std::sync::atomic::Ordering::Relaxed))
    }

    #[cfg(not(feature = "encode"))]
    fn encoded_frames(&self) -> Option<u64> {
        None
    }
}

impl App {
    /// Called from `periodic_update`: retry a failed start, or restart a dead or stalled capture.
    pub(super) fn watch_capture(&mut self) {
        if self.shared.is_some() {
            watch(self, Instant::now());
        }
    }

    /// Called from `periodic_update`: replace the capture when asked (`restart_if_asked`).
    pub(super) fn restart_capture_if_needed(&mut self) {
        if self.shared.is_some() {
            restart_if_asked(self, Instant::now());
        }
    }
}

#[cfg(test)]
#[path = "capture_watchdog_tests.rs"]
mod tests;
