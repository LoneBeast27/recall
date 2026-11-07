//! Edit writes (autosave, the sidecar beside the clip) on a thread of their own: an export or a
//! slow listing on the io thread never holds them up (autosave read "Saving edits…" for a whole
//! export), and closing Recall waits for the ones still queued (a flush sent just before the
//! window closed was lost with the process, and the clip was never marked Edited).
use std::sync::mpsc::{Receiver, Sender};
use std::sync::{Condvar, Mutex};
use std::time::{Duration, Instant};

use super::{Done, Job, PathGate, gated_clip};
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

/// Queues an edit write on the edits thread. False when the thread is gone.
pub(super) fn send(edits: &Sender<Job>, job: Job) -> bool {
    EDITS.begin();
    let sent = edits.send(job).is_ok();
    if !sent {
        EDITS.end();
    }
    sent
}

/// The edits thread: one write at a time, in the order they were sent. Each job leaves `gate`
/// once it finished, success or not (`Workers::job` entered it), so a delete of that clip
/// waits for it and no longer.
pub(super) fn run(jobs: Receiver<Job>, tx: Sender<Done>, gate: PathGate) {
    run_with(jobs, tx, gate, write);
}

/// One edit write and its answer.
fn write(job: Job) -> Option<Done> {
    match job {
        Job::SaveProject(info, plan, fps, export) => Some(Done::Saved(info.path.clone(), backend::save_project(&info, &plan, fps, &export))),
        Job::FlushProject(info, plan, fps, export, ours) => Some(Done::Flushed(info.path.clone(), backend::flush_project(&info, &plan, fps, &export, ours))),
        _ => None,
    }
}

/// [`run`] with the write passed in. A write that panics answers an error for its clip and the
/// thread keeps going (review RI-5): before, the thread ended with the gate still entered, so
/// later saves were dropped with no message and a delete of that clip waited the gate limit.
fn run_with(jobs: Receiver<Job>, tx: Sender<Done>, gate: PathGate, write: impl Fn(Job) -> Option<Done>) {
    while let Ok(job) = jobs.recv() {
        let gated = gated_clip(&job);
        let lost = failed(&job);
        let done = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| write(job))).unwrap_or(lost);
        if let Some(key) = gated {
            gate.leave(&key);
        }
        EDITS.end();
        if let Some(done) = done {
            let _ = tx.send(done);
        }
    }
}

/// The answer for a write that panicked.
fn failed(job: &Job) -> Option<Done> {
    const WHY: &str = "Recall hit an internal error while saving these edits";
    match job {
        Job::SaveProject(info, ..) => Some(Done::Saved(info.path.clone(), Err(WHY.to_owned()))),
        Job::FlushProject(info, ..) => Some(Done::Flushed(info.path.clone(), Err(WHY.to_owned()))),
        _ => None,
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

    #[test]
    fn a_delete_of_a_clip_whose_flush_ran_on_the_edits_thread_goes_at_once() {
        // library x editor: the edits thread runs saves and flushes; a delete waits for them
        // through the PathGate, so the thread must leave it (else every delete of an edited clip
        // waited 600 s). The clip does not exist, so the flush fails fast and writes nothing.
        let info = ClipInfo { path: r"Z:\recall-gate-test\gone.mp4".into(), duration: 1.0, width: 1, height: 1, video_fps: 60.0, source_bytes: 1, source_modified_ms: 1, audio: Vec::new(), keyframes: Vec::new() };
        let key = crate::services::library::clip_key(&info.path);
        let gate = PathGate::default();
        let (jobs, rx) = std::sync::mpsc::channel::<Job>();
        let (done_tx, done) = std::sync::mpsc::channel::<Done>();
        let thread_gate = gate.clone();
        let thread = std::thread::spawn(move || run(rx, done_tx, thread_gate));
        gate.enter(key.clone());
        assert!(send(&jobs, Job::FlushProject(info.clone(), EditPlan::default(), 60, ExportSettings::default(), true)));
        match done.recv_timeout(Duration::from_secs(10)).expect("the flush answers") {
            Done::Flushed(clip, result) => {
                assert_eq!(clip, info.path, "Done::Flushed names the clip (library marks it Edited)");
                assert!(result.is_err());
            }
            _ => panic!("expected Done::Flushed"),
        }
        let started = Instant::now();
        let (ran_tx, ran) = std::sync::mpsc::channel();
        let waited = gate.run_when_clear(vec![key], || {}, move || ran_tx.send(()).unwrap());
        assert!(ran.recv_timeout(Duration::from_secs(2)).is_ok(), "the delete ran (waited: {waited})");
        assert!(started.elapsed() < Duration::from_secs(2), "a delete after the flush does not wait for the gate limit");
        drop(jobs);
        thread.join().unwrap();
    }

    #[test]
    fn a_write_that_panics_leaves_the_gate_and_the_thread_keeps_writing() {
        // RI-5: a panic in a save ended the thread with the gate entered: later saves were lost
        // and a delete of that clip waited the 600 s gate limit.
        let info = ClipInfo { path: r"Z:\recall-panic-test\p.mp4".into(), duration: 1.0, width: 1, height: 1, video_fps: 60.0, source_bytes: 1, source_modified_ms: 1, audio: Vec::new(), keyframes: Vec::new() };
        let key = crate::services::library::clip_key(&info.path);
        let gate = PathGate::default();
        let (jobs, rx) = std::sync::mpsc::channel::<Job>();
        let (done_tx, done) = std::sync::mpsc::channel::<Done>();
        let (thread_gate, calls) = (gate.clone(), std::sync::atomic::AtomicUsize::new(0));
        let thread = std::thread::spawn(move || {
            run_with(rx, done_tx, thread_gate, move |job| {
                if calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst) == 0 {
                    panic!("test: the first write panics");
                }
                match job {
                    Job::SaveProject(info, ..) => Some(Done::Saved(info.path, Ok("saved".into()))),
                    _ => None,
                }
            })
        });
        for _ in 0..2 {
            gate.enter(key.clone());
            assert!(send(&jobs, Job::SaveProject(info.clone(), EditPlan::default(), 60, ExportSettings::default())));
        }
        let answers: Vec<bool> = (0..2)
            .map(|_| match done.recv_timeout(Duration::from_secs(10)).expect("every write answers") {
                Done::Saved(clip, result) => clip == info.path && result.is_ok(),
                _ => panic!("expected Done::Saved"),
            })
            .collect();
        assert_eq!(answers, vec![false, true], "the panicked write answers an error; the next one still runs");
        assert!(gate.is_clear(&[key]), "both writes left the gate");
        drop(jobs);
        thread.join().unwrap();
    }
}
