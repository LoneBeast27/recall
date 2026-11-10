//! `status.save_writing`: what the saver is writing now, read on the main thread at
//! each status line from the saver's own counters (`save_progress.rs`: steps from the
//! audio encode on, packet bytes muxed; no file stat), plus the saves queued behind it.
//! A controller holds engine restarts while the steps keep growing (`saver.rs` aborts
//! the save in flight when the engine quits). Also one job's turn on the saver
//! thread ([`Worker::take`]), between which the run loop counts jobs down.

use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::atomic::Ordering::Relaxed;

use crate::protocol::SaveWriting;

use super::{SaveJob, SaverControl, Worker, QUIT_BEFORE_START};

impl SaverControl {
    /// `None` when no save is in the saver.
    pub fn writing(&self) -> Option<SaveWriting> {
        let jobs = self.progress.jobs.load(Relaxed);
        if jobs == 0 {
            return None;
        }
        let (step, bytes) = self.progress.read();
        Some(SaveWriting { bytes, queued: jobs - 1, step })
    }
}

impl Worker {
    /// One job: failed at once after a quit, else written (a panicking stage fails it).
    pub(super) fn take(&mut self, job: &SaveJob) {
        if self.abort.load(Relaxed) {
            self.fail(QUIT_BEFORE_START);
            return;
        }
        self.progress.new_job();
        let _entered = super::super::save_progress::enter(Some(self.progress.clone()));
        let outcome = catch_unwind(AssertUnwindSafe(|| self.process(job)));
        if outcome.is_err() {
            // A panicking stage must not kill the saver or leave a half file.
            if let Some(part) = self.current.lock().unwrap_or_else(|p| p.into_inner()).take() {
                let _ = std::fs::remove_file(part);
            }
            self.fail("internal error while writing the clip");
        }
    }

    /// A save failed: answered, unless the drain already answered every save in flight
    /// when it gave them up (`SaveProgress::given_up`, design-detect-release P6).
    pub(super) fn fail(&self, reason: &str) {
        if !self.progress.given_up.load(Relaxed) {
            self.events.save_failed(reason);
        }
    }
}
