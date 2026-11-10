//! Stopping the saver at quit (moved out of `saver.rs`). A plain quit waits most of its
//! budget for the saves, then aborts the one in flight. An orphaned engine (its controller
//! crashed) finishes a save in flight instead, but holds on only while one is actually in
//! flight and still making progress ([`OrphanHold`]): it keeps the engine mutex all that
//! time, and the next controller's engine cannot record until it is free.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering::Relaxed};
use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use super::save_progress::SaveProgress;
use crate::protocol::SaveWriting;
use super::saver::Shutdown;
use crate::stdin_reader::ORPHAN_SAVE_BUDGET;

/// An orphan's save that made no progress this long is wedged (a hung encoder or drive).
pub(crate) const ORPHAN_STALL: Duration = Duration::from_secs(120);
/// How often an orphan's wait looks at the save's progress.
const ORPHAN_POLL: Duration = Duration::from_secs(1);

/// Wait for the saver after its queue sender is dropped. Saves run normally for most of
/// `budget` (an orphan: on while [`OrphanHold`] says so); for the last share (<= 500 ms)
/// the in-flight mux is aborted so its `.part` is removed rather than orphaned.
pub(super) fn stop_saver(
    done: &Receiver<()>,
    abort: &AtomicBool,
    current: &Mutex<Option<PathBuf>>,
    budget: Duration,
    orphan: Option<&SaveProgress>,
) -> Shutdown {
    let reserve = (budget / 2).min(Duration::from_millis(500));
    let drained = match orphan {
        None => wait_done(done, budget - reserve),
        Some(progress) => wait_orphan(done, progress, budget - reserve, |save_writing| {
            crate::protocol::emit(&crate::protocol::Outbound::SaveProgress { save_writing });
        }),
    };
    if drained {
        return Shutdown::Drained;
    }
    abort.store(true, Relaxed);
    if wait_done(done, reserve) {
        return Shutdown::Aborted;
    }
    if let Some(part) = current.lock().unwrap_or_else(|p| p.into_inner()).take() {
        let _ = std::fs::remove_file(part);
    }
    Shutdown::TimedOut
}

fn wait_done(done: &Receiver<()>, timeout: Duration) -> bool {
    !matches!(done.recv_timeout(timeout), Err(RecvTimeoutError::Timeout))
}

/// An orphan's wait: true once the saver ended, false when the hold lets go. Each poll with
/// a save in flight is `report`ed (`save_progress`): the event loop that sent `status` has
/// ended, and a released engine's controller must see the save still writing.
fn wait_orphan(done: &Receiver<()>, progress: &SaveProgress, normal: Duration, mut report: impl FnMut(SaveWriting)) -> bool {
    let mut hold = OrphanHold::new(Instant::now(), progress.read().0, normal);
    loop {
        if wait_done(done, ORPHAN_POLL.min(normal.max(Duration::from_millis(1)))) {
            return true;
        }
        let jobs = progress.jobs.load(Relaxed);
        if jobs > 0 {
            let (step, bytes) = progress.read();
            report(SaveWriting { bytes, queued: jobs - 1, step });
        }
        if !hold.keep(progress.jobs.load(Relaxed), progress.read().0, Instant::now()) {
            return false;
        }
    }
}

/// Whether an orphaned engine keeps waiting for its saver: the `normal` budget always;
/// past it only while a save is in flight (`jobs > 0`) and its step counter moved within
/// [`ORPHAN_STALL`]; never past [`ORPHAN_SAVE_BUDGET`].
#[derive(Debug, Clone, Copy)]
pub(crate) struct OrphanHold {
    started: Instant,
    normal: Duration,
    steps: u64,
    moved: Instant,
}

impl OrphanHold {
    pub(crate) fn new(now: Instant, steps: u64, normal: Duration) -> Self {
        Self { started: now, normal, steps, moved: now }
    }

    /// One look at `now`: `jobs` in flight, the saver's step counter.
    pub(crate) fn keep(&mut self, jobs: u32, steps: u64, now: Instant) -> bool {
        let held = now.saturating_duration_since(self.started);
        if held < self.normal {
            return true;
        }
        if held >= ORPHAN_SAVE_BUDGET || jobs == 0 {
            return false;
        }
        if steps != self.steps {
            (self.steps, self.moved) = (steps, now);
        }
        now.saturating_duration_since(self.moved) < ORPHAN_STALL
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_orphan_holds_the_engine_only_while_a_save_makes_progress() {
        // R6-overlay-2: the orphan keeps the engine mutex while it waits.
        let t0 = Instant::now();
        let s = Duration::from_secs;
        let normal = s(3);
        let mut idle = OrphanHold::new(t0, 0, normal);
        assert!(idle.keep(0, 0, t0 + s(1)), "the normal budget always");
        assert!(!idle.keep(0, 0, t0 + s(4)), "nothing in flight: exit after the normal budget");

        let mut busy = OrphanHold::new(t0, 0, normal);
        for sec in 1..=600u64 {
            assert!(busy.keep(1, sec, t0 + s(sec)), "a save still moving at {sec} s holds on");
        }
        let mut hung = OrphanHold::new(t0, 5, normal);
        assert!(hung.keep(1, 5, t0 + s(119)));
        assert!(!hung.keep(1, 5, t0 + s(120)), "no progress for 120 s: a hung encoder, let go");

        let mut late = OrphanHold::new(t0, 0, normal);
        assert!(late.keep(1, 0, t0 + s(60)) && !late.keep(0, 0, t0 + s(61)), "the save answered: exit");

        let mut long = OrphanHold::new(t0, 0, normal);
        assert!(!long.keep(1, 1, t0 + ORPHAN_SAVE_BUDGET), "never past the 30 min cap");
    }

    #[test]
    fn a_draining_save_reports_its_progress_every_poll() {
        // R2-detect-r10-p2-1: after stop_capture no `status` line goes out; the drain says
        // the save still writes, so the controller does not give up on it at 120 s.
        let progress = SaveProgress::default();
        progress.jobs.store(2, Relaxed);
        let (tx, done) = std::sync::mpsc::channel::<()>();
        let ender = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(2_500));
            drop(tx);
        });
        let mut seen = Vec::new();
        assert!(wait_orphan(&done, &progress, Duration::from_millis(10), |w| seen.push(w)), "the saver ended");
        ender.join().unwrap();
        assert!(seen.len() >= 2, "one per poll: {seen:?}");
        assert_eq!(seen[0].queued, 1, "the one writing plus one behind it");
    }

    thread_local! {
        static EMITTED: std::cell::RefCell<Vec<SaveWriting>> = const { std::cell::RefCell::new(Vec::new()) };
    }

    #[test]
    fn stop_saver_emits_save_progress_on_the_protocol_while_it_drains() {
        // R2-detect-r10-p3-2: the live quit path (`stop_saver`, not `wait_orphan` with a test
        // closure) puts `save_progress` on the protocol. The observer sees every emit; this
        // thread's own lines are kept (other tests emit on their threads).
        crate::protocol::set_observer(|msg| {
            if let crate::protocol::Outbound::SaveProgress { save_writing } = msg {
                EMITTED.with(|e| e.borrow_mut().push(*save_writing));
            }
        });
        let progress = SaveProgress::default();
        progress.jobs.store(1, Relaxed);
        let (tx, done) = std::sync::mpsc::channel::<()>();
        let ender = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(2_500));
            drop(tx);
        });
        let (abort, current) = (AtomicBool::new(false), Mutex::new(None));
        let outcome = stop_saver(&done, &abort, &current, Duration::from_millis(20), Some(&progress));
        ender.join().unwrap();
        assert_eq!(outcome, Shutdown::Drained);
        let seen = EMITTED.with(|e| e.borrow().len());
        assert!(seen >= 2, "save_progress lines while draining: {seen}");
    }

    #[test]
    fn an_orphan_with_nothing_to_save_and_a_hung_encoder_exits_promptly() {
        // The encoder thread still holds the queue (a hung NVENC): the saver never ends.
        let (events, _sink) = super::super::saver::Events::capture(true);
        let mux: super::super::saver::MuxFn = Box::new(|_, _, _, _| unreachable!("no job submitted"));
        let (queue, control) = super::super::saver::spawn(events, mux, Box::new(|_, _, _, _| {}));
        let started = Instant::now();
        assert_eq!(control.shutdown_orphan(Duration::from_millis(600)), Shutdown::TimedOut);
        assert!(started.elapsed() < Duration::from_secs(3), "{:?} (was 30 min)", started.elapsed());
        drop(queue);
    }
}
