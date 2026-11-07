//! Edit writes (autosave, the sidecar beside the clip) on a thread of their own: an export or a
//! slow listing on the io thread never holds them up (autosave read "Saving edits…" for a whole
//! export), and closing Recall waits for the ones still queued (a flush sent just before the
//! window closed was lost with the process, and the clip was never marked Edited).
use std::sync::mpsc::{Receiver, Sender};
use std::sync::{Condvar, Mutex};
use std::time::{Duration, Instant};

use super::{Done, Job};
use crate::backend;

/// Which thread a job runs on.
#[derive(Debug, PartialEq)]
pub(crate) enum Lane {
    Io,
    Edits,
}

pub(crate) fn lane(job: &Job) -> Lane {
    match job {
        Job::SaveProject(..) | Job::FlushProject(..) => Lane::Edits,
        _ => Lane::Io,
    }
}

/// Edit writes sent and not finished yet.
pub(crate) struct Pending {
    count: Mutex<usize>,
    idle: Condvar,
}

impl Pending {
    pub(crate) const fn new() -> Self {
        Self { count: Mutex::new(0), idle: Condvar::new() }
    }

    pub(crate) fn begin(&self) {
        *self.count.lock().unwrap_or_else(|e| e.into_inner()) += 1;
    }

    pub(crate) fn end(&self) {
        let mut count = self.count.lock().unwrap_or_else(|e| e.into_inner());
        *count = count.saturating_sub(1);
        if *count == 0 {
            self.idle.notify_all();
        }
    }

    /// True once nothing is pending; false when `limit` passed first.
    pub(crate) fn wait_idle(&self, limit: Duration) -> bool {
        let deadline = Instant::now() + limit;
        let mut count = self.count.lock().unwrap_or_else(|e| e.into_inner());
        while *count > 0 {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return false;
            }
            count = self.idle.wait_timeout(count, left).unwrap_or_else(|e| e.into_inner()).0;
        }
        true
    }
}

static EDITS: Pending = Pending::new();

/// Queues an edit write on the edits thread.
pub(super) fn send(edits: &Sender<Job>, job: Job) {
    EDITS.begin();
    if edits.send(job).is_err() {
        EDITS.end();
    }
}

/// The edits thread: one write at a time, in the order they were sent.
pub(super) fn run(jobs: Receiver<Job>, tx: Sender<Done>) {
    while let Ok(job) = jobs.recv() {
        let done = match job {
            Job::SaveProject(info, plan, fps, export) => Some(Done::Saved(info.path.clone(), backend::save_project(&info, &plan, fps, &export))),
            Job::FlushProject(info, plan, fps, export, ours) => Some(Done::Flushed(info.path.clone(), backend::flush_project(&info, &plan, fps, &export, ours))),
            _ => None,
        };
        EDITS.end();
        if let Some(done) = done {
            let _ = tx.send(done);
        }
    }
}

/// App close: the running export, then the edit writes queued (`limit` for those). False when
/// some were left.
pub fn finish_writes(wait_export: impl FnOnce(), limit: Duration) -> bool {
    finish(&EDITS, wait_export, limit)
}

fn finish(pending: &Pending, wait_export: impl FnOnce(), limit: Duration) -> bool {
    // A flush queued behind the export on its disk lane can only start once the export ends.
    wait_export();
    pending.wait_idle(limit)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::edit_model::{ClipInfo, EditPlan, ExportSettings};
    use std::sync::Arc;

    #[test]
    fn edit_writes_do_not_queue_behind_an_export_or_a_listing() {
        // E9: autosave waited for the whole export on the one io thread.
        let info = ClipInfo { path: "a.mp4".into(), duration: 1.0, width: 1, height: 1, video_fps: 60.0, source_bytes: 1, source_modified_ms: 1, audio: Vec::new(), keyframes: Vec::new() };
        let (plan, settings) = (EditPlan::default(), ExportSettings::default());
        assert_eq!(lane(&Job::SaveProject(info.clone(), plan.clone(), 60, settings.clone())), Lane::Edits);
        assert_eq!(lane(&Job::FlushProject(info.clone(), plan.clone(), 60, settings.clone(), true)), Lane::Edits);
        assert_eq!(lane(&Job::Export(info, plan, 60, settings)), Lane::Io);
        assert_eq!(lane(&Job::List), Lane::Io);
    }

    #[test]
    fn close_waits_for_a_queued_flush_and_gives_up_after_its_limit() {
        // E7: the flush queued on leaving the editor was dropped when the process exited.
        let pending = Arc::new(Pending::new());
        assert!(pending.wait_idle(Duration::ZERO), "nothing queued: no wait");
        pending.begin();
        assert!(!pending.wait_idle(Duration::from_millis(30)), "a write still running holds the close up to the limit");
        let writer = Arc::clone(&pending);
        let started = Instant::now();
        let thread = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(80));
            writer.end();
        });
        assert!(pending.wait_idle(Duration::from_secs(10)), "the close waits for the write");
        assert!(started.elapsed() >= Duration::from_millis(70));
        thread.join().unwrap();
    }

    #[test]
    fn close_waits_for_the_export_before_it_times_the_edit_writes() {
        // R2-2: a flush queued behind an export on the HDD lane gave up after 30 s, then the
        // process exited the moment the export ended, while the flush was only starting.
        let pending = Arc::new(Pending::new());
        pending.begin();
        let writer = Arc::clone(&pending);
        let export = move || {
            std::thread::sleep(Duration::from_millis(100));
            writer.end(); // the lane is free: the queued flush runs
        };
        assert!(finish(&pending, export, Duration::from_millis(10)), "the limit counts from the end of the export");
    }
}
