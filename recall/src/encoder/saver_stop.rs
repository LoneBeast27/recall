//! Stopping the saver at quit (moved out of `saver.rs`). A plain quit waits most of its
//! budget for the saves, then aborts the one in flight. An orphaned or released engine
//! (its controller crashed, or sent `stop_capture`) finishes a save in flight instead, but
//! holds on only while one is actually in flight and still making progress ([`OrphanHold`]):
//! it keeps the engine mutex all that time, and the next controller's engine cannot record
//! until it is free.
//!
//! design-detect-release P5/P6: the drain sends a heartbeat (`save_progress`) at once and on
//! every poll, alive or stalled, and every hold duration runs on the awake-only clock
//! (`awake_clock`), so a sleep during the drain ages nothing. The engine is the only judge of
//! a stalled save.
//!
//! Never lose a clip: when the drain lets go of a save (stalled, past the 30 min budget, or a
//! plain quit's budget) it records why ([`SaveProgress::set_stop_reason`]) and aborts; the
//! mux then finishes the file with what it wrote and the saver answers `saved` with that
//! reason as `partial`. A saver that does not stop within the reserve is answered here
//! (`save_failed`, one per save still unanswered) and its `.part` stays on disk for the next
//! start's recovery; nothing here deletes it.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering::Relaxed};
use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::sync::Mutex;
use std::time::Duration;

use super::save_progress::SaveProgress;
use super::saver::Shutdown;
use crate::awake_clock::{Awake, AwakeClock, Clock};
use crate::protocol::{Outbound, SaveWriting};
use crate::stdin_reader::ORPHAN_SAVE_BUDGET;

/// An orphan's save that made no progress this long (awake) is wedged (a hung encoder or drive).
pub(crate) const ORPHAN_STALL: Duration = Duration::from_secs(120);
/// How often an orphan's wait looks at the save's progress (and sends its heartbeat).
const ORPHAN_POLL: Duration = Duration::from_secs(1);
/// Why the engine gave up a save that stopped moving (product wording, design P6).
pub(crate) const STALLED_REASON: &str = "The save stopped making progress";
/// Why the engine gave up a save still writing at the 30 min budget.
pub(crate) const BUDGET_REASON: &str = "The save was still writing after 30 minutes";
/// Why a plain quit cut a save short.
pub(crate) const QUIT_REASON: &str = "Recall quit before the save finished";
/// Added to the engine's own answer when the saver did not stop: its file is not deleted.
pub(crate) const KEPT_FOR_RECOVERY: &str = "what was written is kept for recovery at the next start";

/// Wait for the saver after its queue sender is dropped. Saves run normally for most of
/// `budget` (an `orphan`: on while [`OrphanHold`] says so); for the last share (<= 500 ms)
/// the in-flight mux is aborted and keeps what it wrote (see the module doc).
pub(super) fn stop_saver(
    done: &Receiver<()>,
    abort: &AtomicBool,
    current: &Mutex<Option<PathBuf>>,
    budget: Duration,
    progress: &SaveProgress,
    orphan: bool,
) -> Shutdown {
    stop_saver_on(done, abort, current, budget, progress, orphan, &AwakeClock, ORPHAN_POLL)
}

/// [`stop_saver`] on `clock`, polling the orphan's saves every `poll` (tests: a fake clock).
#[allow(clippy::too_many_arguments)]
pub(in crate::encoder) fn stop_saver_on(
    done: &Receiver<()>,
    abort: &AtomicBool,
    current: &Mutex<Option<PathBuf>>,
    budget: Duration,
    progress: &SaveProgress,
    orphan: bool,
    clock: &dyn Clock,
    poll: Duration,
) -> Shutdown {
    let reserve = (budget / 2).min(Duration::from_millis(500));
    let (drained, reason) = if orphan {
        let beat = |save_writing, stalled_for| crate::protocol::emit(&Outbound::SaveProgress { save_writing, stalled_for });
        match wait_orphan(done, progress, budget - reserve, clock, poll, beat) {
            Hold::Drained => (true, QUIT_REASON),
            Hold::LetGo(reason) => (false, reason.unwrap_or(QUIT_REASON)),
        }
    } else {
        (wait_done(done, budget - reserve), QUIT_REASON)
    };
    if drained {
        return Shutdown::Drained;
    }
    progress.set_stop_reason(reason);
    abort.store(true, Relaxed);
    if wait_done(done, reserve) {
        return Shutdown::Aborted;
    }
    give_up(progress, reason);
    if let Some(part) = current.lock().unwrap_or_else(|p| p.into_inner()).as_ref() {
        let message = format!("saver did not stop in time ({reason}); {} is kept for recovery at the next start", part.display());
        crate::protocol::emit(&Outbound::EncoderLog { message });
    }
    Shutdown::TimedOut
}

/// The saver did not stop (design P6): every save still unanswered gets its `save_failed`
/// here, and the saver sends no answer after it (`SaveProgress::give_up_unanswered`), so the
/// controller never sees the engine exit with an unanswered save.
fn give_up(progress: &SaveProgress, reason: &str) {
    let reason = format!("{reason}; {KEPT_FOR_RECOVERY}");
    progress.give_up_unanswered(|| crate::protocol::emit(&Outbound::SaveFailed { reason: reason.clone(), rejected: false }));
}

fn wait_done(done: &Receiver<()>, timeout: Duration) -> bool {
    !matches!(done.recv_timeout(timeout), Err(RecvTimeoutError::Timeout))
}

/// How an orphan's wait ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Hold {
    /// The saver ended.
    Drained,
    /// The hold let go with nothing in flight (`None`) or of a save (`Some(reason)`).
    LetGo(Option<&'static str>),
}

/// An orphan's wait. The heartbeat goes out at once (poll 0: the event loop that sent
/// `status` has ended, and a released engine's controller must hear from it within its
/// heartbeat window) and then on every poll, the save alive or stalled.
fn wait_orphan(done: &Receiver<()>, progress: &SaveProgress, normal: Duration, clock: &dyn Clock, poll: Duration, mut report: impl FnMut(Option<SaveWriting>, u64)) -> Hold {
    let mut hold = OrphanHold::new(clock.awake(), progress.read().0, normal);
    loop {
        let jobs = progress.jobs.load(Relaxed);
        let (step, bytes) = progress.read();
        let now = clock.awake();
        let verdict = hold.keep(jobs, step, now);
        let writing = (jobs > 0).then_some(SaveWriting { bytes, queued: jobs.saturating_sub(1), step });
        report(writing, hold.stalled_for(now).as_secs());
        if let Some(reason) = verdict {
            return Hold::LetGo(reason);
        }
        if wait_done(done, poll.min(normal.max(Duration::from_millis(1)))) {
            return Hold::Drained;
        }
    }
}

/// Whether an orphaned engine keeps waiting for its saver: the `normal` budget always;
/// past it only while a save is in flight (`jobs > 0`) and its step counter moved within
/// [`ORPHAN_STALL`]; never past [`ORPHAN_SAVE_BUDGET`]. All on the awake-only clock.
#[derive(Debug, Clone, Copy)]
pub(crate) struct OrphanHold {
    started: Awake,
    normal: Duration,
    steps: u64,
    moved: Awake,
}

impl OrphanHold {
    pub(crate) fn new(now: Awake, steps: u64, normal: Duration) -> Self {
        Self { started: now, normal, steps, moved: now }
    }

    /// One look at `now`: `jobs` in flight, the saver's step counter. `None` = keep waiting;
    /// `Some(None)` = let go, nothing in flight; `Some(Some(reason))` = let go of a save.
    pub(crate) fn keep(&mut self, jobs: u32, steps: u64, now: Awake) -> Option<Option<&'static str>> {
        if steps != self.steps {
            (self.steps, self.moved) = (steps, now);
        }
        let held = now.saturating_duration_since(self.started);
        if held < self.normal {
            return None;
        }
        if jobs == 0 {
            return Some(None);
        }
        if held >= ORPHAN_SAVE_BUDGET {
            return Some(Some(BUDGET_REASON));
        }
        (self.stalled_for(now) >= ORPHAN_STALL).then_some(Some(STALLED_REASON))
    }

    /// Awake time since the step counter last moved.
    pub(crate) fn stalled_for(&self, now: Awake) -> Duration {
        now.saturating_duration_since(self.moved)
    }
}

#[cfg(test)]
#[path = "saver_stop_tests.rs"]
mod tests;
