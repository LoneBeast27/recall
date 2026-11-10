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

use super::super::in_progress;

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
/// Never lose a clip: the save is listed for recovery (`in_progress`) before its `.part` exists.
/// A failed save's `.part` goes to the Recycle Bin only when the mux wrote no packet into it
/// (logged); otherwise it stays, with its packet index, for the next start's recovery.
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
    // Listed before the `.part` exists: a save cut off now is recovered at the next start.
    let index = super::super::replay_index::slot();
    if let Err(e) = in_progress::add(in_progress::Entry::replay(part, Path::new(&job.path), index.as_deref())) {
        log(&format!("saver: {} not listed for recovery ({e})", part.display()));
    }
    match mux(part, job, abort, timing) {
        Ok(muxed) => {
            let started = Instant::now();
            let renamed = std::fs::rename(part, target);
            timing.rename_ms = ms(started.elapsed());
            match renamed {
                Ok(()) => {
                    unlist(part, log);
                    Ok(muxed)
                }
                // Finished but not renamed: the file is whole, the next start adopts it (no index needed).
                Err(e) => {
                    log(&format!("saver: {} is finished but could not be renamed ({e}); kept for recovery at the next start", part.display()));
                    let _ = in_progress::update(part, |entry| entry.index = None);
                    Err(format!("finalize {}: {e}", target.display()))
                }
            }
        }
        Err(reason) => {
            if super::super::part_recover::after_failed_mux(part, timing.part_packets, &reason, log) {
                keep_index(part, index.as_deref(), log);
            } else {
                unlist(part, log);
            }
            Err(reason)
        }
    }
}

fn unlist(part: &Path, log: &dyn Fn(&str)) {
    if let Err(e) = in_progress::remove(part) {
        log(&format!("saver: {} still listed for recovery ({e})", part.display()));
    }
}

/// A save kept for recovery keeps its packet index: out of the slot the next save overwrites.
fn keep_index(part: &Path, slot: Option<&Path>, log: &dyn Fn(&str)) {
    let Some(slot) = slot.filter(|s| super::super::replay_index::idx_of(s).exists()) else { return };
    match super::super::replay_index::claim(slot) {
        Ok(own) => {
            let _ = in_progress::update(part, |entry| entry.index = Some(own.to_string_lossy().into_owned()));
        }
        Err(e) => log(&format!("saver: packet index for {} not kept ({e})", part.display())),
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
