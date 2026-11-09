//! Small writes (the few-KB sidecar beside a clip) on a disk lane that a long task holds: an
//! export runs its whole engine on the output drive's lane, so a sidecar queued behind it waited
//! for the whole export ("Saving edits…", and lost when Recall closed first). A brief task that
//! has not started within its wait runs on the caller's thread instead: one small write next to
//! the long task's stream costs a seek, not minutes.
use std::sync::{Arc, Mutex};
use std::time::Duration;

use super::lane::{hdd_lane, Lane, Priority};

/// How long a brief task waits for its lane before it runs on the caller's thread.
pub const BRIEF_WAIT: Duration = Duration::from_millis(1500);

/// [`super::run_io`] for a small task: on the lane of `path` when the lane gets to it within
/// [`BRIEF_WAIT`], else here. Runs `work` exactly once.
pub fn run_io_brief<T: Send + 'static>(path: &std::path::Path, priority: Priority, work: impl FnOnce() -> T + Send + 'static) -> Result<T, String> {
    match hdd_lane(path) {
        Some(lane) => run_brief(&lane, priority, BRIEF_WAIT, work),
        None => Ok(work()),
    }
}

pub(super) fn run_brief<T: Send + 'static>(lane: &Lane, priority: Priority, wait: Duration, work: impl FnOnce() -> T + Send + 'static) -> Result<T, String> {
    if lane.is_current() {
        return Ok(work());
    }
    // Whoever takes the work first runs it: the lane, or this thread once `wait` has passed.
    let slot = Arc::new(Mutex::new(Some(work)));
    let take = |slot: &Mutex<Option<_>>| slot.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).take();
    let (tx, rx) = std::sync::mpsc::sync_channel(1);
    let queued = Arc::clone(&slot);
    lane.submit(priority, Box::new(move || {
        if let Some(work) = take(&queued) {
            let _ = tx.send(work());
        }
    }));
    match rx.recv_timeout(wait) {
        Ok(value) => Ok(value),
        Err(_) => match take(&slot) {
            Some(work) => Ok(work()),
            // The lane started it just now: wait for its result.
            None => rx.recv().map_err(|_| "disk lane task failed".to_string()),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Instant;

    #[test]
    fn a_small_write_does_not_wait_for_a_long_task_on_its_lane() {
        // R2-2: the sidecar flush queued behind an export that held the HDD lane.
        let lane = Lane::spawn("recall-test-brief", false, Duration::from_secs(60));
        let (started_tx, started_rx) = std::sync::mpsc::channel();
        lane.submit(Priority::Interactive, Box::new(move || {
            let _ = started_tx.send(());
            std::thread::sleep(Duration::from_millis(600));
        }));
        started_rx.recv().unwrap();
        let runs = Arc::new(AtomicUsize::new(0));
        let counted = Arc::clone(&runs);
        let at = Instant::now();
        let value = run_brief(&lane, Priority::Interactive, Duration::from_millis(50), move || {
            counted.fetch_add(1, Ordering::SeqCst);
            7
        });
        assert_eq!(value, Ok(7));
        assert!(at.elapsed() < Duration::from_millis(400), "waited {:?} behind the long task", at.elapsed());
        std::thread::sleep(Duration::from_millis(700));
        assert_eq!(runs.load(Ordering::SeqCst), 1, "the lane must not run it a second time");
    }

    #[test]
    fn a_free_lane_runs_the_small_write_itself() {
        let lane = Lane::spawn("recall-test-brief-free", false, Duration::from_secs(60));
        let on_lane = run_brief(&lane, Priority::Interactive, Duration::from_secs(5), || std::thread::current().name().map(str::to_owned));
        assert_eq!(on_lane, Ok(Some("recall-test-brief-free".to_owned())));
    }
}
