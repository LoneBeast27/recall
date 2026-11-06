//! Capture watchdog: a capture that failed to start, or a WGC capture whose thread
//! ended (the handler returned an error, e.g. a frame read failed), is started again
//! from `periodic_update` with a backoff of 1, 2, 5, 10 s, then every 10 s. Each try
//! and each death is logged. The decision is [`Backoff`], a pure helper; [`watch`] runs it
//! over a [`CaptureSlot`] (the [`App`], or a fake in the tests).
//!
//! It also restarts a capture the encoder or the capture thread asked to replace
//! ([`restart_if_asked`], also with no capture held), on its own backoff. [`tick`] runs
//! both, so one periodic tick opens at most one capture (one D3D11 device).
//!
//! It acts only on evidence that capture is dead or must change: the thread ended (a
//! handler error, or the WGC item closed: windows-capture posts WM_QUIT on
//! `GraphicsCaptureItem.Closed`), a start failed, or capture / the encoder asked
//! (`restart_reason`). No new frames on their own never restart anything: the encoder
//! is VFR and WGC sends no repeats, so a load screen, a shader compile or a pause menu
//! sends none while the capture is alive.
//!
//! While the target is minimized, the session is locked or the display is off
//! (`capture::screen`) nothing is reaped, started or restarted: a start then fails or
//! gets no frames, and each restart opens a new D3D11 device. The wait resumes when the
//! user is back.

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
    /// Restarts `restart_if_asked` made in a row and the last one's time (its backoff).
    asked_restarts: u32,
    last_asked_restart: Option<Instant>,
    /// A request that came inside that backoff, kept for when it runs out.
    deferred: Option<&'static str>,
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

    /// A held capture may be replaced on request now (1, 2, 5, 10 s between replacements;
    /// one that ran [`STABLE`] starts over). Counts the replacement when it says yes.
    pub(crate) fn may_replace(&mut self, now: Instant) -> bool {
        if let Some(last) = self.last_asked_restart {
            let since = now.duration_since(last);
            if since < Self::delay(self.asked_restarts) {
                return false;
            }
            if since >= STABLE {
                self.asked_restarts = 0;
            }
        }
        self.asked_restarts = self.asked_restarts.saturating_add(1);
        self.last_asked_restart = Some(now);
        true
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
    /// A held capture is about to be replaced for `why` (the replay buffer restarts).
    fn replacing(&mut self, _why: &'static str) {}
}

/// `periodic_update`'s capture step: [`watch`], then [`restart_if_asked`] only when watch
/// opened nothing. A start already uses the current source, so a request pending then is
/// taken and dropped (it would tear the new capture down at once).
pub(crate) fn tick(slot: &mut impl CaptureSlot, now: Instant) -> Step {
    let step = watch(slot, now);
    match step {
        Step::Idle => restart_if_asked(slot, now),
        Step::Start => {
            if let Some(why) = slot.backoff().deferred.take().or_else(|| slot.restart_reason()) {
                eprintln!("[recall] capture restart asked {why}; the start just made uses it");
            }
        }
        Step::Reap => {}
    }
    step
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
        Step::Idle => {}
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

/// `restart_capture_if_needed`: replace the capture when asked, at most on
/// [`Backoff::may_replace`]'s backoff (a request inside it waits for it). With no capture
/// held the request is taken too (the next start must not be torn down by it) and a start
/// is due now unless a backoff wait runs. Nothing while the screen is away (the request waits).
pub(crate) fn restart_if_asked(slot: &mut impl CaptureSlot, now: Instant) {
    if slot.screen().away() {
        return;
    }
    let (has_capture, _) = slot.held();
    let Some(why) = slot.backoff().deferred.take().or_else(|| slot.restart_reason()) else { return };
    if has_capture {
        if !slot.backoff().may_replace(now) {
            slot.backoff().deferred = Some(why);
            return;
        }
        eprintln!("[recall] capture restarts {why}; the replay buffer restarts");
        slot.replacing(why);
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

    /// HDR turning on under a Bgra8 capture is told to the controller (its notice).
    fn replacing(&mut self, why: &'static str) {
        if why == crate::capture::format::HDR_TURNED_ON && self.config.json {
            crate::protocol::emit(&crate::protocol::Outbound::BufferRestarted { reason: "hdr_on" });
        }
    }

}

impl App {
    /// Called from `periodic_update`: retry a failed start, restart a dead capture, or
    /// replace it when asked ([`tick`]: at most one new capture per call).
    pub(super) fn watch_capture(&mut self) {
        if self.shared.is_some() {
            tick(self, Instant::now());
        }
    }
}

#[cfg(test)]
#[path = "capture_watchdog_tests.rs"]
mod tests;
