//! A named device stream follows its pick (R7-deep-audio-1): on every device notice it picks
//! again (`stream::repick`, the same A4 rule as the open) and ends at once when the pick is no
//! longer the endpoint it has open. The loop then reopens on the new pick, or waits in silence
//! when no single endpoint matches (0 or 2+). So a track never keeps recording a sibling endpoint
//! after its own device returns, renumbered or not, with or without a saved id, input or output.
//!
//! The re-pick enumerates every endpoint, so in the engine it runs on its own short-lived thread
//! (`Run::Thread`) and the capture loop only polls its answer (R7-deep-audio-4): a slow audiosrv
//! during a USB arrival never stalls the capture thread past its 100 ms buffer. One re-pick at a
//! time; notices that come meanwhile, or while the stream was opening (R7-deep-audio-3), queue
//! one more.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, Receiver, TryRecvError};
use std::sync::Arc;

use super::capture_loop::{Ended, Notices};
use super::endpoints::id_ok;

/// What a re-pick compares: the endpoint id when the source saved a well-formed one, else the
/// endpoint's name. A source with no saved id never reads endpoint ids (it cannot be own, and
/// the name alone tells its endpoints apart: two with one name match none).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum Key {
    Id(String),
    Name(String),
}

/// The key of the endpoint `name` (its id from `id`, read only with a saved id). An unreadable id
/// falls back to the name, the same way on the open and on every re-pick.
pub(super) fn key_for(saved_id: &str, name: &str, id: impl FnOnce() -> Option<String>) -> Key {
    let id = id_ok(saved_id).then(id).flatten().map(|id| id.trim().to_owned()).filter(|id| id_ok(id));
    id.map_or_else(|| Key::Name(name.trim().to_owned()), Key::Id)
}

/// What the pick opens now: None when no single endpoint matches (A4: the track waits).
pub(super) type Probe = Arc<dyn Fn() -> Option<Key> + Send + Sync>;

/// Where a re-pick runs: its own thread (the engine), or inline (tests on a fake clock).
#[derive(Clone, Copy, Debug)]
pub(super) enum Run {
    Thread,
    /// Tests only.
    #[cfg_attr(not(test), allow(dead_code))]
    Inline,
}

/// The rebind of one open named stream.
pub(super) struct Rebind {
    opened: Key,
    probe: Probe,
    run: Run,
    /// The answer of the re-pick in flight.
    running: Option<Receiver<Option<Key>>>,
    /// A notice came that no re-pick has seen yet.
    queued: bool,
    /// Taken while a re-pick thread of this track runs, shared by its streams (p9 gate item 3):
    /// at most one runs at a time, also after a stream ended with one still in flight.
    slot: Arc<AtomicBool>,
}

impl Rebind {
    /// End the stream now: a re-pick after a device notice opens another endpoint, or none.
    /// Never waits on the re-pick (`Run::Thread`).
    pub(super) fn check(&mut self, notices: &dyn Notices) -> Option<Ended> {
        self.queued |= notices.take_devices_changed();
        if self.running.is_none() && self.queued {
            // One still running (a slow audiosrv): skip; the queued notice waits for the slot.
            self.running = self.start();
            self.queued = self.running.is_none();
        }
        let answer = match self.running.as_ref()?.try_recv() {
            Ok(answer) => answer,
            Err(TryRecvError::Empty) => return None,
            Err(TryRecvError::Disconnected) => {
                // The re-pick died without an answer (a failed spawn, a panic): keep the stream
                // and try again on the next notice.
                self.running = None;
                return None;
            }
        };
        self.running = None;
        let why = match answer {
            Some(key) if key == self.opened => return None,
            Some(_) => "a device change moved the pick to another endpoint",
            None => "a device change left no single endpoint by this name; waiting",
        };
        Some(Ended { why: why.to_owned(), at_once: true })
    }

    /// Start a re-pick; None while this track's slot is taken.
    fn start(&self) -> Option<Receiver<Option<Key>>> {
        let (tx, rx) = channel();
        let probe = self.probe.clone();
        match self.run {
            Run::Inline => {
                let _ = tx.send(probe());
            }
            Run::Thread => {
                if self.slot.compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst).is_err() {
                    return None;
                }
                let slot = Slot(self.slot.clone());
                // A failed spawn drops `tx` and the slot: the answer is lost, as for a panic.
                let _ = std::thread::Builder::new().name("recall-audio-repick".into()).spawn(move || {
                    let _slot = slot;
                    let _ = tx.send(probe());
                });
            }
        }
        Some(rx)
    }
}

/// Frees a track's re-pick slot when its thread ends, a panic included.
struct Slot(Arc<AtomicBool>);

impl Drop for Slot {
    fn drop(&mut self) {
        self.0.store(false, Ordering::SeqCst);
    }
}

/// The rebind for a stream that opened `opened` for `device`: every named device (input or
/// output, own or not). A source with no device records the Windows default and is watched
/// instead (`capture_loop::watch_for`).
pub(super) fn rebind_for(device: &str, opened: Key, probe: Probe, (run, slot): (Run, Arc<AtomicBool>)) -> Option<Rebind> {
    (!device.trim().is_empty()).then(|| Rebind { opened, probe, run, running: None, queued: false, slot })
}

#[cfg(test)]
#[path = "rebind_tests.rs"]
mod tests;
