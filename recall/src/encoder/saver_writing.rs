//! `status.save_writing`: what the saver is writing now, read on the main thread at
//! each status line. One stat of the in-progress `.part` (0 bytes before the file
//! exists, e.g. while the audio still encodes), plus the saves queued behind it. A
//! controller holds engine restarts while the bytes keep growing (`saver.rs` aborts
//! the save in flight when the engine quits). Also one job's turn on the saver
//! thread ([`Worker::take`]), between which the run loop counts jobs down.

use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::atomic::Ordering::Relaxed;

use crate::protocol::SaveWriting;

use super::{SaveJob, SaverControl, Worker, QUIT_BEFORE_START};

impl SaverControl {
    /// `None` when no save is in the saver.
    pub fn writing(&self) -> Option<SaveWriting> {
        let jobs = self.jobs.load(Relaxed);
        if jobs == 0 {
            return None;
        }
        let part = self.current.lock().unwrap_or_else(|p| p.into_inner()).clone();
        let bytes = part.and_then(|p| std::fs::metadata(p).ok()).map_or(0, |m| m.len());
        Some(SaveWriting { bytes, queued: jobs - 1 })
    }
}

impl Worker {
    /// One job: failed at once after a quit, else written (a panicking stage fails it).
    pub(super) fn take(&mut self, job: &SaveJob) {
        if self.abort.load(Relaxed) {
            self.events.save_failed(QUIT_BEFORE_START);
            return;
        }
        let outcome = catch_unwind(AssertUnwindSafe(|| self.process(job)));
        if outcome.is_err() {
            // A panicking stage must not kill the saver or leave a half file.
            if let Some(part) = self.current.lock().unwrap_or_else(|p| p.into_inner()).take() {
                let _ = std::fs::remove_file(part);
            }
            self.events.save_failed("internal error while writing the clip");
        }
    }
}
