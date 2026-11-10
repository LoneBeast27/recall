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

/// Endpoint changes the loop hears about: `notify.rs` (IMMNotificationClient) in the engine,
/// injected in tests. Each call takes the notice, so it is acted on once.
pub(super) trait Notices {
    /// A device was added, removed or changed state since the last call: a waiting track tries
    /// its device now instead of at the end of its backoff.
    fn take_devices_changed(&self) -> bool;
    /// The Windows default endpoint's id for this track's direction, when it changed since the
    /// last call (the latest of several changes).
    fn take_default(&self) -> Option<String>;
}

/// No notices (registration failed): plain backoff, and a default source stays where it opened.
pub(super) struct NoNotices;

impl Notices for NoNotices {
    fn take_devices_changed(&self) -> bool {
        false
    }

    fn take_default(&self) -> Option<String> {
        None
    }
}

/// The device notices a track registers (`register`, run on its capture thread). A
/// process-loopback track follows a process, not an endpoint, so it registers none.
pub(super) fn notices_for<N>(loopback: bool, register: impl FnOnce() -> Result<N, String>) -> Option<Result<N, String>> {
    (!loopback).then(register)
}

/// One open capture stream.
pub(super) trait Capture {
    /// The endpoint or process it captures, for the status.
    fn endpoint(&self) -> String;
    /// The endpoint is the source's own saved device (`endpoints::opened_own`): only then may
    /// a line-in skip Alt+M. Unsure = false, so the input follows the mic hotkey.
    fn own(&self) -> bool {
        false
    }
    /// An input's unsure exact-name sibling (A4 ruling, `endpoints::Choice::Unsure`), with its
    /// endpoint id when read: it records silence until a fresh Alt+M (`hold.rs`).
    fn unsure(&self) -> Option<Option<String>> {
        None
    }
    /// Capture into `ring` until the stream fails or must move; the stream is stopped on return.
    fn run(&mut self, ring: &TrackRing, level: &mut Leveller, clock: &dyn Clock, notices: &dyn Notices) -> Ended;
}

/// A source with no device recording the Windows default: follows the default when it moves.
/// Named sources are never watched (they never open another device).
#[derive(Debug)]
pub(super) struct Watch {
    id: String,
}

impl Watch {
    /// The stream should end and reopen at once: the default moved to another endpoint. No
    /// default at all is not a move (the stream reports the lost device itself).
    pub(super) fn check(&mut self, notices: &dyn Notices) -> Option<Ended> {
        let id = notices.take_default()?;
        (id != self.id).then(|| Ended { why: "the Windows default moved to another device".to_owned(), at_once: true })
    }
}

/// The watch for a stream that opened `device`: only a source with no device, on the default
/// endpoint `default_id`.
pub(super) fn watch_for(device: &str, default_id: Option<String>) -> Option<Watch> {
    device.trim().is_empty().then_some(())?;
    Some(Watch { id: default_id? })
}

/// A device source that does not open at start still gets its track (silence) and waits: a
/// named device for itself, a source on the Windows default for a default to appear (the
/// headset is off, or an app holds the default in exclusive mode). Not for process loopback (a
/// bad target is reported, never waited on), or `RECALL_AUDIO_EXACT` (tests: a missing test
/// endpoint means no track, as before).
pub(super) fn waits_when_absent(loopback: bool, exact: bool) -> bool {
    !loopback && !exact
}

/// The status of a device that has not appeared yet (`sources::opened`).
pub(super) fn waiting_label(device: &str) -> String {
    match device.trim() {
        "" => "Windows default (none yet; waiting)".to_owned(),
        name => format!("{name} (not connected; waiting)"),
    }
}

/// Silence up to the clock (the source sends nothing right now).
pub(super) fn silence_to_clock(ring: &TrackRing, level: &mut Leveller, clock: &dyn Clock) {
    let frames = level.idle(clock.elapsed());
    if frames > 0 {
        super::packet::push_silence(ring, frames as usize * ring.channels as usize);
    }
}

impl TrackRing {
    /// The ring's length in frames (the most silence a `Leveller` may owe).
    pub(super) fn capacity_frames(&self) -> u64 {
        (self.capacity / self.channels.max(1) as usize) as u64
    }
}

/// Whether the stream just opened (false once it ends) is the source's own saved device. A named
/// input on another endpoint says so in the log: it follows Alt+M (R5-deep-audio-1).
fn set_opened_own(ring: &TrackRing, endpoint: &str, own: bool, opening: bool) {
    ring.opened_own.store(own, std::sync::atomic::Ordering::Relaxed);
    if opening && ring.input && !ring.on_default && !own {
        eprintln!("[recall] audio track '{}': {endpoint} is not the saved device; it follows the mic hotkey", ring.name);
    }
}

fn set_endpoint(ring: &TrackRing, text: String) {
    if let Ok(mut g) = ring.endpoint.lock() {
        *g = text;
    }
}

/// Open, capture, and when the device or stream is lost, keep the track level with silence
/// and reopen. `open(reopen)`: `reopen` is false only for the first attempt. A named device
/// only ever opens by its own name. The first open answers `init_tx`: a failure there ends the
/// track unless `wait_absent`, which starts the track as "not connected; waiting" and keeps
/// trying, at once when `notices` says a device came or went.
pub(super) fn capture_loop<C: Capture>(
    ring: &TrackRing,
    device: &str,
    wait_absent: bool,
    mut open: impl FnMut(bool) -> Result<C, String>,
    init_tx: Sender<Result<(), String>>,
    clock: &mut dyn Clock,
    notices: &dyn Notices,
) {
    let mut level = Leveller::new(ring.rate).with_cap(ring.capacity_frames()); // a sleep owes at most one ring
    let mut init_tx = Some(init_tx);
    // Failed reopens back off: 2 s, 4 s, 8 s, then every 10 s, and back to 2 s once it opens.
    let mut failures = 0u32;
    loop {
        if !wait_startup_hold(ring, &mut level, clock, &mut init_tx) {
            return;
        }
        let mut wait = true;
        match open(init_tx.is_none()) {
            Ok(mut stream) => {
                failures = 0;
                let endpoint = stream.endpoint();
                // The endpoint's real name: a renumbered "4- X" for a saved "3- X" shows as itself.
                eprintln!("[recall] audio track '{}': opened {endpoint}", ring.name);
                set_opened_own(ring, &endpoint, stream.own(), true);
                match stream.unsure() {
                    Some(id) => {
                        ring.live.start_hold(&endpoint, id);
                        set_endpoint(ring, super::endpoints::unsure_notice(&endpoint));
                    }
                    None => set_endpoint(ring, endpoint.clone()),
                }
                if let Some(tx) = init_tx.take() {
                    let _ = tx.send(Ok(()));
                }
                // A device notice that came while the stream was opening is kept for the stream's
                // own checks (its rebind re-picks once, R7-deep-audio-3), not dropped as stale.
                let queued = Queued { missed: std::cell::Cell::new(notices.take_devices_changed()), notices };
                let ended = stream.run(ring, &mut level, &*clock, &queued);
                set_opened_own(ring, &endpoint, false, false);
                ring.live.end_hold();
                // Nor do the ones that came while it was live (another device came or went):
                // the first reopen after this loss keeps its backoff.
                notices.take_devices_changed();
                // Up to the clock, and out of live sound: the next stream starts with silence for
                // the time its open takes, so the track never lags by the switch.
                silence_to_clock(ring, &mut level, &*clock);
                eprintln!("[recall] audio track '{}': {}; reopening", ring.name, ended.why);
                set_endpoint(ring, format!("{endpoint} (lost: {}; reopening)", ended.why));
                wait = !ended.at_once;
            }
            Err(e) => {
                // Two devices share the name, or only its case matches: the status says so.
                if super::endpoints::is_notice(&e) && ring.endpoint() != e {
                    eprintln!("[recall] audio track '{}': {e}", ring.name);
                    set_endpoint(ring, e.clone());
                }
                if let Some(tx) = init_tx.take() {
                    if !wait_absent {
                        let _ = tx.send(Err(e));
                        return;
                    }
                    let label = if super::endpoints::is_notice(&e) { e.clone() } else { waiting_label(device) };
                    eprintln!("[recall] audio track '{}': {e}; {label}", ring.name);
                    set_endpoint(ring, label);
                    let _ = tx.send(Ok(()));
                }
                failures = failures.saturating_add(1);
            }
        }
        if !wait_to_reopen(ring, &mut level, clock, notices, if wait { reopen_wait(failures) } else { Duration::ZERO }) {
            return;
        }
    }
}

/// The endpoint text of a track whose start is held (merge-gate F4/F7).
pub(crate) const STARTUP_HELD: &str = "Held: waiting for the recorder's settings";

/// While the startup hold is set (until the controller's `sync`) no stream opens, input or output
/// (merge-gate F4): the first open reads the device the last applied `set_source` names, never
/// a stale one or a twin the user rejected. The track starts (`init_tx`) as held, level with
/// silence. False: the clock ended the loop (tests only).
fn wait_startup_hold(ring: &TrackRing, level: &mut Leveller, clock: &mut dyn Clock, init_tx: &mut Option<Sender<Result<(), String>>>) -> bool {
    while ring.startup_held() {
        // The safe state: no device stream exists while held.
        if let Some(tx) = init_tx.take() {
            set_endpoint(ring, STARTUP_HELD.to_owned());
            let _ = tx.send(Ok(()));
        }
        if !clock.pause(TICK) {
            return false;
        }
        silence_to_clock(ring, level, &*clock);
    }
    true
}

/// The notices a stream runs with: the device notice taken at its open (`missed`) comes back on
/// its first `take_devices_changed`.
struct Queued<'a> {
    missed: std::cell::Cell<bool>,
    notices: &'a dyn Notices,
}

impl Notices for Queued<'_> {
    fn take_devices_changed(&self) -> bool {
        self.missed.replace(false) | self.notices.take_devices_changed()
    }

    fn take_default(&self) -> Option<String> {
        self.notices.take_default()
    }
}

/// Silence for up to `wait`, cut short when a device comes or goes. False: the clock ended
/// the loop (tests only).
fn wait_to_reopen(ring: &TrackRing, level: &mut Leveller, clock: &mut dyn Clock, notices: &dyn Notices, wait: Duration) -> bool {
    let until = clock.elapsed() + wait;
    while clock.elapsed() < until {
        if notices.take_devices_changed() {
            return true;
        }
        if !clock.pause(TICK) {
            return false;
        }
        silence_to_clock(ring, level, &*clock);
    }
    wait > Duration::ZERO || clock.pause(Duration::ZERO)
}

#[cfg(test)]
#[path = "capture_loop_tests.rs"]
pub(super) mod tests;

#[cfg(test)]
#[path = "own_device_tests.rs"]
mod own_device_tests;

#[cfg(test)]
#[path = "id_pick_loop_tests.rs"]
mod id_pick_loop_tests;

#[cfg(test)]
#[path = "startup_hold_tests.rs"]
mod startup_hold_tests;

#[cfg(test)]
#[path = "alloc_tests.rs"]
mod alloc_tests;
