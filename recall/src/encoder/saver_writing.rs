//! `status.save_writing`: what the saver is writing now, read on the main thread at
//! each status line from the saver's own counters (`save_progress.rs`: steps from the
//! audio encode on, packet bytes muxed; no file stat), plus the saves queued behind it.
//! A controller holds engine restarts while the steps keep growing (`saver.rs` aborts
//! the save in flight when the engine quits). Also one job's turn on the saver
//! thread ([`Worker::take`]), between which the run loop counts jobs down.

use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::atomic::Ordering::Relaxed;

use std::path::Path;
use std::sync::atomic::AtomicBool;
use std::time::Instant;

use crate::protocol::SaveWriting;

use super::{ms, part_path, Muxed, SaveJob, SaverControl, Timing, Worker, QUIT_BEFORE_START};

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
            // A panicking stage must not kill the saver. Its `.part` is not deleted (never
            // lose a clip): the next start's recovery adopts or keeps it.
            if let Some(part) = self.current.lock().unwrap_or_else(|p| p.into_inner()).take() {
                self.events.log(&format!("saver: a stage panicked; {} is kept for recovery at the next start", part.display()));
            }
            self.fail("internal error while writing the clip");
        }
    }

    /// A save failed: answered once, unless it already was (a panic after `saved`) or the
    /// drain answered every save in flight when it gave them up (design-detect-release P6).
    pub(super) fn fail(&self, reason: &str) {
        self.progress.answer(|| self.events.save_failed(reason));
    }
}

/// Mux into `part`, then rename it to the job's path. A clip path only ever holds a
/// finished file (one cut short is finished with what was written, `Muxed::partial`).
/// Never lose a clip: a failed save's `.part` is deleted only when the mux wrote no packet
/// into it (logged); otherwise it stays for the next start's recovery (`part_recover`).
pub(crate) fn write_clip<F>(
    part: &Path,
    job: &SaveJob,
    mux: &mut F,
    abort: &AtomicBool,
    timing: &mut Timing,
) -> Result<Muxed, String>
where
    F: FnMut(&Path, &SaveJob, &AtomicBool, &mut Timing) -> Result<Muxed, String> + ?Sized,
{
    write_clip_to(part, Path::new(&job.path), job, mux, abort, timing, &|line: &str| eprintln!("[recall] {line}"))
}

/// [`write_clip`] renaming to `target` (the staged file, or the job's path).
pub(crate) fn write_clip_to<F>(
    part: &Path,
    target: &Path,
    job: &SaveJob,
    mux: &mut F,
    abort: &AtomicBool,
    timing: &mut Timing,
    log: &dyn Fn(&str),
) -> Result<Muxed, String>
where
    F: FnMut(&Path, &SaveJob, &AtomicBool, &mut Timing) -> Result<Muxed, String> + ?Sized,
{
    timing.part_packets = 0;
    match mux(part, job, abort, timing) {
        Ok(muxed) => {
            let started = Instant::now();
            let renamed = std::fs::rename(part, target);
            timing.rename_ms = ms(started.elapsed());
            match renamed {
                Ok(()) => Ok(muxed),
                // Finished but not renamed: the file is whole, the next start adopts it.
                Err(e) => {
                    log(&format!("saver: {} is finished but could not be renamed ({e}); kept for recovery at the next start", part.display()));
                    Err(format!("finalize {}: {e}", target.display()))
                }
            }
        }
        Err(reason) => {
            super::super::part_recover::after_failed_mux(part, timing.part_packets, &reason, log);
            Err(reason)
        }
    }
}

/// Synchronous save through the production mux + rename path (smoke test).
pub(crate) fn write_now(job: &SaveJob) -> Result<Muxed, String> {
    let mut timing = job.timing;
    write_clip(
        &part_path(&job.path),
        job,
        &mut super::super::mux::produce,
        &AtomicBool::new(false),
        &mut timing,
    )
}
