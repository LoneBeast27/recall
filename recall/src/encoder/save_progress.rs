//! The save being written, as the saver's own counters (`status.save_writing`): `steps`
//! goes up for every AAC frame encoded and every packet muxed, `bytes` by the packet
//! bytes muxed. So the audio encode, which runs before the `.part` exists, shows
//! progress too, and the status line reads two atomics instead of a file stat on the
//! engine's main thread (a stalled volume no longer blocks it).
//!
//! The saver thread enters its job's counters ([`enter`]); the mux and the audio encode
//! threads it starts report into them with [`step`] / [`wrote`] (no counters entered:
//! nothing happens, e.g. the smoke test's synchronous save).

use std::cell::RefCell;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering::Relaxed};
use std::sync::Arc;

/// One saver's counters, shared with the main thread's status line.
#[derive(Debug, Default)]
pub(crate) struct SaveProgress {
    /// Saves handed over and not yet answered (counted up before the hand-over, down
    /// after the answer went out).
    pub jobs: AtomicU32,
    steps: AtomicU64,
    bytes: AtomicU64,
}

impl SaveProgress {
    /// A new job starts: its bytes count from 0 (the steps only grow).
    pub fn new_job(&self) {
        self.bytes.store(0, Relaxed);
    }

    /// (steps, bytes) now.
    pub fn read(&self) -> (u64, u64) {
        (self.steps.load(Relaxed), self.bytes.load(Relaxed))
    }
}

thread_local! {
    static CURRENT: RefCell<Option<Arc<SaveProgress>>> = const { RefCell::new(None) };
}

/// The counters this thread reports into until the guard drops.
pub(crate) struct Entered {
    previous: Option<Arc<SaveProgress>>,
}

impl Drop for Entered {
    fn drop(&mut self) {
        let previous = self.previous.take();
        CURRENT.with(|c| *c.borrow_mut() = previous);
    }
}

/// Report into `progress` on this thread (the saver thread, an audio encode thread).
pub(crate) fn enter(progress: Option<Arc<SaveProgress>>) -> Entered {
    Entered { previous: CURRENT.with(|c| std::mem::replace(&mut *c.borrow_mut(), progress)) }
}

/// The counters this thread reports into, to hand to threads it starts.
pub(crate) fn current() -> Option<Arc<SaveProgress>> {
    CURRENT.with(|c| c.borrow().clone())
}

fn with_current(f: impl FnOnce(&SaveProgress)) {
    CURRENT.with(|c| {
        if let Some(p) = c.borrow().as_deref() {
            f(p);
        }
    });
}

/// One unit of work done (an AAC frame encoded).
pub(crate) fn step() {
    with_current(|p| {
        p.steps.fetch_add(1, Relaxed);
    });
}

/// One packet of `bytes` muxed.
pub(crate) fn wrote(bytes: u64) {
    with_current(|p| {
        p.steps.fetch_add(1, Relaxed);
        p.bytes.fetch_add(bytes, Relaxed);
    });
}
