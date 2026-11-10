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
use std::sync::{Arc, Mutex};

/// One saver's counters, shared with the main thread's status line.
#[derive(Debug, Default)]
pub(crate) struct SaveProgress {
    /// Saves handed over and not yet answered (counted up before the hand-over, down
    /// after the answer went out).
    pub jobs: AtomicU32,
    /// One answer per save, whoever sends it (the saver, or the drain that gave up on it).
    answers: Mutex<Answers>,
    /// Why the drain stopped the saver (stall, budget, quit): a save it cut short is kept
    /// with this as its `partial` reason.
    stop_reason: Mutex<Option<&'static str>>,
    steps: AtomicU64,
    bytes: AtomicU64,
}

#[derive(Debug, Default)]
struct Answers {
    /// The drain answered every save still in the saver (`saver_stop`): the saver sends no
    /// answer of its own after it.
    given_up: bool,
    /// The job in hand was answered and is not yet counted down from `jobs`.
    answered: bool,
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

    fn lock(&self) -> std::sync::MutexGuard<'_, Answers> {
        self.answers.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// The saver answers the job in hand with `send`: once per job, and never after the
    /// drain gave the saves up. False when it was not sent.
    pub fn answer(&self, send: impl FnOnce()) -> bool {
        let mut answers = self.lock();
        if answers.given_up || answers.answered {
            return false;
        }
        send();
        answers.answered = true;
        true
    }

    /// The job in hand is done (after its answer): it leaves `jobs`.
    pub fn settle(&self) {
        let mut answers = self.lock();
        self.jobs.fetch_sub(1, Relaxed);
        answers.answered = false;
    }

    /// The drain answers every save still unanswered with `send` (`saver_stop`), and the
    /// saver sends nothing after it. Returns how many.
    pub fn give_up_unanswered(&self, send: impl Fn()) -> u32 {
        let mut answers = self.lock();
        let open = self.jobs.load(Relaxed).saturating_sub(answers.answered as u32);
        answers.given_up = true;
        for _ in 0..open {
            send();
        }
        open
    }

    /// The drain answered the saves itself.
    #[cfg(test)]
    pub fn given_up(&self) -> bool {
        self.lock().given_up
    }

    /// Why the drain stopped the saver (before its abort).
    pub fn set_stop_reason(&self, reason: &'static str) {
        *self.stop_reason.lock().unwrap_or_else(|p| p.into_inner()) = Some(reason);
    }

    /// The reason a save cut short by the drain is kept with.
    pub fn stop_reason(&self) -> Option<&'static str> {
        *self.stop_reason.lock().unwrap_or_else(|p| p.into_inner())
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
