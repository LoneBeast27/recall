//! The storage schedule with a test channel (no worker thread, no disk reads).

use super::*;
use crate::record_storage::GIB;
use std::sync::mpsc::TryRecvError;

const DIR: &str = r"E:\Clips";
/// 120 s at 25 Mbit/s.
const BUFFER: u64 = 375_000_000;

fn janitor() -> (Janitor, Receiver<Job>, Instant) {
    let (tx, rx) = channel();
    let t0 = Instant::now();
    (Janitor::with_sender(tx, Some(PathBuf::from(r"F:\Recall\cache\staging")), t0), rx, t0)
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
    assert_eq!(job.free.unwrap().threshold, 12 * GIB, "max(2 x buffer, 5 GiB)");
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
    let cap = Job { dir: DIR.into(), cap_gb: 500, preview_for: None, free: None };
    let free_only = Job { dir: DIR.into(), cap_gb: 0, preview_for: None, free: Some(expected_check(0)) };
    let merged = coalesce(Some(cap.clone()), free_only.clone());
    assert_eq!((merged.cap_gb, merged.free), (500, Some(expected_check(0))));
    assert_eq!(coalesce(Some(free_only), cap.clone()), cap, "the newest cap run wins");
}

#[test]
fn free_warnings_fire_once_then_rearm() {
    let mut low = FreeWarner::default();
    let c = expected_check(BUFFER);
    let short: FreeFn = |d| Some(if d.starts_with("F:") { GIB } else { 900 * GIB });
    let first = free_warnings(&mut low, &c, short);
    assert_eq!(first, vec![free_space::free_event("F:", GIB, 5 * GIB)]);
    assert!(free_warnings(&mut low, &c, short).is_empty(), "not every run");
    assert!(free_warnings(&mut low, &c, |_| Some(50 * GIB)).is_empty());
    assert_eq!(free_warnings(&mut low, &c, short).len(), 1, "re-armed after recovering");
}
