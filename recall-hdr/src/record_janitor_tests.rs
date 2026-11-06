//! The storage schedule with a test channel (no worker thread, no disk reads).

use super::*;
use crate::record_storage::GIB;
use std::sync::mpsc::TryRecvError;

const DIR: &str = r"E:\Clips";
/// 120 s at 25 Mbit/s.
const BUFFER: u64 = 375_000_000;

/// Labels from the path text, never touching a disk.
fn by_letter(dir: &Path) -> Option<crate::win_volume::Volume> {
    let label = dir.to_string_lossy()[..2].to_ascii_uppercase();
    Some(crate::win_volume::Volume { id: label.clone(), label })
}

/// A volume lookup stuck on a sleeping NAS / spun-down disk.
fn blocks(dir: &Path) -> Option<crate::win_volume::Volume> {
    std::thread::sleep(Duration::from_millis(1500));
    by_letter(dir)
}

fn janitor_with(resolve: Resolve) -> (Janitor, Receiver<Job>, Instant) {
    let (tx, rx) = channel();
    let t0 = Instant::now();
    (Janitor::with_sender(tx, Some(PathBuf::from(r"F:\Recall\cache\staging")), t0, resolve), rx, t0)
}

fn janitor() -> (Janitor, Receiver<Job>, Instant) {
    janitor_with(by_letter)
}

fn expected_check(buffer: u64) -> FreeCheck {
    FreeCheck::new(Path::new(DIR), Some(Path::new(r"F:\Recall\cache\staging")), buffer)
}

#[test]
fn free_space_is_checked_soon_after_start_without_a_cap_run() {
    let (mut j, rx, t0) = janitor();
    j.tick(DIR, 0, BUFFER, t0 + Duration::from_secs(1));
    assert_eq!(rx.try_recv(), Err(TryRecvError::Empty));
    assert_eq!(j.next_deadline(), Some(t0 + START_CHECK), "the pump wakes for it");
    j.tick(DIR, 500, BUFFER, t0 + START_CHECK);
    let job = rx.try_recv().expect("a start check");
    assert_eq!(job.cap_gb, 0, "free space only: the cap waits for its own schedule");
    assert_eq!(job.free, Some(expected_check(BUFFER)));
    j.tick(DIR, 500, BUFFER, t0 + START_CHECK * 2);
    assert_eq!(rx.try_recv(), Err(TryRecvError::Empty), "once");
}

#[test]
fn every_save_and_hourly_run_checks_free_space_even_with_no_cap() {
    let (mut j, rx, t0) = janitor();
    let buffer = 6 * GIB;
    j.saved(t0);
    j.tick(DIR, 0, buffer, t0 + AFTER_SAVE);
    let job = rx.try_recv().expect("an after-save run with cap 0");
    assert_eq!((job.cap_gb, job.free.clone()), (0, Some(expected_check(buffer))));
    assert_eq!(job.free.unwrap().threshold, 12 * GIB, "max(2 x buffer, 5 GB)");
    j.tick(DIR, 250, buffer, t0 + AFTER_SAVE + HOURLY);
    let job = rx.try_recv().expect("the hourly run");
    assert_eq!(job.cap_gb, 250);
    assert!(job.free.is_some());
    assert_eq!(rx.try_recv(), Err(TryRecvError::Empty));
}

#[test]
fn run_now_with_no_cap_still_checks_free_space() {
    let (mut j, rx, t0) = janitor();
    j.run_now(DIR, 0, t0);
    let job = rx.try_recv().expect("cap 0 still sends the free-space check");
    assert_eq!(job.cap_gb, 0);
    assert!(job.free.is_some());
}

#[test]
fn a_free_only_job_never_drops_a_queued_cap_run() {
    let cap = Job { dir: DIR.into(), cap_gb: 500, free_only: false, preview_for: None, free: None };
    let free_only = Job { dir: DIR.into(), cap_gb: 0, free_only: true, preview_for: None, free: Some(expected_check(0)) };
    let merged = coalesce(Some(cap.clone()), free_only.clone());
    assert_eq!((merged.cap_gb, merged.free), (500, Some(expected_check(0))));
    assert_eq!(coalesce(Some(free_only), cap.clone()), cap, "the newest cap run wins");
}

#[test]
fn turning_the_limit_off_drops_a_queued_cap_run() {
    // The stale cap job: a run with the old cap is still queued when the user sets the limit to off.
    let (mut j, rx, t0) = janitor();
    j.run_now(DIR, 500, t0);
    j.run_now(r"G:\NewClips", 0, t0);
    let queued = rx.try_recv().unwrap();
    let merged = coalesce(Some(queued), rx.try_recv().unwrap());
    assert_eq!((merged.cap_gb, merged.dir), (0, PathBuf::from(r"G:\NewClips")), "nothing goes to the Recycle Bin under the old cap");
}

#[test]
fn a_blocked_volume_lookup_never_delays_the_pump() {
    let (mut j, rx, t0) = janitor_with(blocks);
    let started = Instant::now();
    j.saved(t0);
    j.tick(DIR, 0, BUFFER, t0 + AFTER_SAVE);
    j.tick(DIR, 0, BUFFER, t0 + START_CHECK);
    j.run_now(DIR, 500, t0 + START_CHECK);
    let took = started.elapsed();
    assert!(took < Duration::from_millis(200), "the pump waited {took:?} on a volume lookup");
    assert_eq!(rx.try_iter().count(), 2, "the after-save run (it replaces the start check) and the run-now job");
}

#[test]
fn free_warnings_fire_once_then_rearm() {
    let mut low = FreeWarner::default();
    let c = expected_check(BUFFER);
    let short: FreeFn = |d| Some(if d.starts_with("F:") { GIB } else { 900 * GIB });
    let first = free_warnings(&mut low, &c, short, by_letter);
    assert_eq!(first, vec![free_space::free_event("F:", GIB, 5_000_000_000)]);
    assert!(free_warnings(&mut low, &c, short, by_letter).is_empty(), "not every run");
    assert!(free_warnings(&mut low, &c, |_| Some(50 * GIB), by_letter).is_empty());
    assert_eq!(free_warnings(&mut low, &c, short, by_letter).len(), 1, "re-armed after recovering");
}
