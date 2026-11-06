//! The open / capture / reopen loop of a device or game track, apart from WASAPI: the capture
//! thread hands it an opener and the wall clock; tests hand it a fake device and a fake clock.
//!
//! The track stays level with the clock the whole time (`Leveller`): silence while the device
//! is gone or not connected yet, sound while it is open. A lost device is reopened every
//! `REOPEN_EVERY`, backing off to `REOPEN_MAX`, by its own name only.

use std::sync::mpsc::Sender;
use std::time::{Duration, Instant};

use super::apps::Leveller;
use super::TrackRing;

/// How long a track waits between attempts to reopen a device or process it lost.
const REOPEN_EVERY: Duration = Duration::from_secs(2);
/// The longest wait between reopen attempts after repeated failures.
const REOPEN_MAX: Duration = Duration::from_secs(10);
/// Step between silence fills while the track waits to reopen.
const TICK: Duration = Duration::from_millis(100);

/// Wait before the next reopen after `failures` failed attempts in a row.
pub(super) fn reopen_wait(failures: u32) -> Duration {
    REOPEN_EVERY.saturating_mul(1 << failures.saturating_sub(1).min(3)).min(REOPEN_MAX)
}

/// The track's time since its capture thread started. `pause` sleeps; a test clock moves on
/// instead, and ends the loop by returning false.
pub(super) trait Clock {
    fn elapsed(&self) -> Duration;
    fn pause(&mut self, step: Duration) -> bool;
}

/// The real clock: never ends the loop.
pub(super) struct WallClock(Instant);

impl WallClock {
    pub(super) fn start() -> Self {
        WallClock(Instant::now())
    }
}

impl Clock for WallClock {
    fn elapsed(&self) -> Duration {
        self.0.elapsed()
    }

    fn pause(&mut self, step: Duration) -> bool {
        std::thread::sleep(step);
        true
    }
}

/// How an open stream ended.
pub(super) struct Ended {
    pub why: String,
    /// Reopen at once (the Windows default moved to another device), not after `REOPEN_EVERY`.
    pub at_once: bool,
}

/// One open capture stream.
pub(super) trait Capture {
    /// The endpoint or process it captures, for the status.
    fn endpoint(&self) -> String;
    /// Capture into `ring` until the stream fails or must move; the stream is stopped on return.
    fn run(&mut self, ring: &TrackRing, level: &mut Leveller, clock: &dyn Clock) -> Ended;
}

/// How often a stream on the Windows default checks whether it should move.
const WATCH_EVERY: Duration = Duration::from_secs(2);

/// What a stream on the Windows default waits for.
#[derive(Clone, Debug, PartialEq)]
pub(super) enum Want {
    /// A source with no device: the default moves away from this endpoint id.
    DefaultMovesFrom(String),
    /// A named device that was missing at start (the stream fell back to the default): it appears.
    Named(String),
}

/// The endpoints as a watch sees them (WASAPI in the engine, a fake in tests).
pub(super) trait Endpoints {
    /// The Windows default endpoint's id now, if there is one.
    fn default_id(&self) -> Option<String>;
    /// An endpoint matching `want` (the recorder's name rule) is connected now.
    fn has(&self, want: &str) -> bool;
}

/// Checks every `WATCH_EVERY` whether a stream on the Windows default should reopen.
#[derive(Debug)]
pub(super) struct Watch {
    want: Want,
    next: Duration,
}

impl Watch {
    pub(super) fn new(want: Want, now: Duration) -> Self {
        Watch { want, next: now + WATCH_EVERY }
    }

    /// The stream should end and reopen at once: the default moved, or the named device
    /// appeared (the reopen takes it by name). No default at all is not a move (the stream
    /// reports the lost device itself).
    pub(super) fn check(&mut self, now: Duration, endpoints: &dyn Endpoints) -> Option<Ended> {
        if now < self.next {
            return None;
        }
        self.next = now + WATCH_EVERY;
        let why = match &self.want {
            Want::DefaultMovesFrom(id) => endpoints.default_id().is_some_and(|now| now != *id).then(|| "the Windows default moved to another device".to_owned()),
            Want::Named(name) => endpoints.has(name).then(|| format!("'{}' connected", name.trim())),
        };
        why.map(|why| Ended { why, at_once: true })
    }
}

/// The watch for a stream that opened `device`: on the Windows default for a source with no
/// device (`default_id`), or on the default because the named device was missing (`fell_back`).
pub(super) fn watch_for(device: &str, fell_back: bool, default_id: Option<String>, now: Duration) -> Option<Watch> {
    let want = if device.trim().is_empty() {
        Want::DefaultMovesFrom(default_id?)
    } else if fell_back {
        Want::Named(device.trim().to_owned())
    } else {
        return None;
    };
    Some(Watch::new(want, now))
}

/// A named device that does not open at start still gets its track (silence) and waits for
/// the device by its own name. Not for process loopback (a bad target is reported, never
/// waited on), a Windows-default source (no name to wait for), or `RECALL_AUDIO_EXACT`
/// (tests: a missing test endpoint means no track, as before).
pub(super) fn waits_when_absent(device: &str, loopback: bool, exact: bool) -> bool {
    !device.trim().is_empty() && !loopback && !exact
}

/// The status of a named device that has not appeared yet (`sources::opened`).
pub(super) fn waiting_label(device: &str) -> String {
    format!("{} (not connected; waiting)", device.trim())
}

/// Silence up to the clock (the source sends nothing right now).
pub(super) fn silence_to_clock(ring: &TrackRing, level: &mut Leveller, clock: &dyn Clock) {
    let frames = level.idle(clock.elapsed());
    if frames > 0 {
        ring.push_capture(&vec![0.0; frames as usize * ring.channels as usize], false);
    }
}

fn set_endpoint(ring: &TrackRing, text: String) {
    if let Ok(mut g) = ring.endpoint.lock() {
        *g = text;
    }
}

/// Open, capture, and when the device or stream is lost, keep the track level with silence
/// and reopen. `open(reopen)`: `reopen` is false only for the first attempt (a named device
/// may then fall back to the Windows default; a reopen never does). The first open answers
/// `init_tx`: a failure there ends the track unless `wait_absent`, which starts the track
/// as "not connected; waiting" and keeps trying.
pub(super) fn capture_loop<C: Capture>(
    ring: &TrackRing,
    device: &str,
    wait_absent: bool,
    mut open: impl FnMut(bool) -> Result<C, String>,
    init_tx: Sender<Result<(), String>>,
    clock: &mut dyn Clock,
) {
    let mut level = Leveller::new(ring.rate);
    let mut init_tx = Some(init_tx);
    // Failed reopens back off: 2 s, 4 s, 8 s, then every 10 s, and back to 2 s once it opens.
    let mut failures = 0u32;
    loop {
        let mut wait = true;
        match open(init_tx.is_none()) {
            Ok(mut stream) => {
                failures = 0;
                let endpoint = stream.endpoint();
                set_endpoint(ring, endpoint.clone());
                if let Some(tx) = init_tx.take() {
                    let _ = tx.send(Ok(()));
                }
                let ended = stream.run(ring, &mut level, &*clock);
                eprintln!("[recall] audio track '{}': {}; reopening", ring.name, ended.why);
                set_endpoint(ring, format!("{endpoint} (lost: {}; reopening)", ended.why));
                wait = !ended.at_once;
            }
            Err(e) => {
                if let Some(tx) = init_tx.take() {
                    if !wait_absent {
                        let _ = tx.send(Err(e));
                        return;
                    }
                    eprintln!("[recall] audio track '{}': {e}; waiting for '{}'", ring.name, device.trim());
                    set_endpoint(ring, waiting_label(device));
                    let _ = tx.send(Ok(()));
                }
                failures = failures.saturating_add(1);
            }
        }
        let until = clock.elapsed() + if wait { reopen_wait(failures) } else { Duration::ZERO };
        while clock.elapsed() < until {
            if !clock.pause(TICK) {
                return;
            }
            silence_to_clock(ring, &mut level, &*clock);
        }
        if !wait && !clock.pause(Duration::ZERO) {
            return;
        }
    }
}

#[cfg(test)]
#[path = "capture_loop_tests.rs"]
mod tests;
