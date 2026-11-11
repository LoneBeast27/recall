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

// ---- Nothing is recycled beyond the user's CURRENT storage limit ----

use crate::record_storage::{ClipFile, Listing, Plan};
use std::sync::Mutex as StdMutex;

const OLD_CLIP: &str = "G__2026-01-01__00-00-00.mp4";

/// A plan that would move one clip (no disk read).
fn plan_one(_: &Path, _: u32) -> Result<(Plan, Listing), String> {
    let clip = ClipFile { name: OLD_CLIP.into(), bytes: GIB, modified: 1 };
    let plan = Plan { remove: vec![OLD_CLIP.into()], freed: GIB, total: 600 * GIB, through: Some(1), next: None };
    Ok((plan, Listing { clips: vec![clip], names: vec![OLD_CLIP.into()], thumbs: Vec::new() }))
}

/// The listing of a folder on a sleeping NAS.
fn slow_plan(dir: &Path, cap: u32) -> Result<(Plan, Listing), String> {
    std::thread::sleep(Duration::from_millis(400));
    plan_one(dir, cap)
}

/// A volume lookup on a sleeping NAS.
fn slow_resolve(dir: &Path) -> Option<crate::win_volume::Volume> {
    std::thread::sleep(Duration::from_millis(400));
    by_letter(dir)
}

fn roomy(_: &Path) -> Option<u64> {
    Some(900 * GIB)
}

static RECYCLED_A: StdMutex<Vec<Instant>> = StdMutex::new(Vec::new());
static RECYCLED_B: StdMutex<Vec<Instant>> = StdMutex::new(Vec::new());
static RECYCLED_C: StdMutex<Vec<Instant>> = StdMutex::new(Vec::new());

fn recycle_a(_: &[PathBuf]) -> Result<(), crate::record_storage::RecycleError> {
    RECYCLED_A.lock().unwrap().push(Instant::now());
    Ok(())
}
fn recycle_b(_: &[PathBuf]) -> Result<(), crate::record_storage::RecycleError> {
    RECYCLED_B.lock().unwrap().push(Instant::now());
    Ok(())
}
fn recycle_c(_: &[PathBuf]) -> Result<(), crate::record_storage::RecycleError> {
    RECYCLED_C.lock().unwrap().push(Instant::now());
    Ok(())
}

fn cap_job() -> Job {
    Job { dir: DIR.into(), cap_gb: 500, free_only: false, preview_for: None, free: Some(expected_check(BUFFER)) }
}

/// Run the batch on its own thread; after 100 ms the user sets "No limit". Returns when that happened.
fn batch_with_limit_off_midway(deps: Deps) -> Instant {
    let current = Arc::new(Mutex::new(Setting { dir: DIR.into(), cap_gb: 500 }));
    let shared = current.clone();
    let worker = std::thread::spawn(move || {
        let (mut low, mut warner) = (FreeWarner::default(), Warner::default());
        run_batch(&cap_job(), &shared, deps, &mut low, &mut warner, &mut |_| {});
    });
    std::thread::sleep(Duration::from_millis(100));
    *current.lock().unwrap() = Setting { dir: DIR.into(), cap_gb: 0 };
    let changed = Instant::now();
    worker.join().unwrap();
    changed
}

#[test]
fn a_slow_free_space_check_never_lets_a_stale_cap_recycle() {
    let deps = Deps { free: roomy, resolve: slow_resolve, plan: plan_one, bin: any_bin, recycle: recycle_a, keep: no_keeps, present: |_| true };
    let changed = batch_with_limit_off_midway(deps);
    let late: Vec<_> = RECYCLED_A.lock().unwrap().iter().filter(|at| **at >= changed).copied().collect();
    assert!(late.is_empty(), "recycled {} time(s) after the limit was turned off", late.len());
}

#[test]
fn a_limit_change_during_the_listing_moves_nothing() {
    let deps = Deps { free: roomy, resolve: by_letter, plan: slow_plan, bin: any_bin, recycle: recycle_b, keep: no_keeps, present: |_| true };
    batch_with_limit_off_midway(deps);
    assert_eq!(RECYCLED_B.lock().unwrap().len(), 0, "the run listed under 500 GB but the limit is now off");
}

#[test]
fn a_job_for_another_folder_or_cap_moves_nothing() {
    let deps = Deps { free: roomy, resolve: by_letter, plan: plan_one, bin: any_bin, recycle: recycle_c, keep: no_keeps, present: |_| true };
    let (mut low, mut warner) = (FreeWarner::default(), Warner::default());
    for now in [Setting { dir: DIR.into(), cap_gb: 0 }, Setting { dir: r"G:\NewClips".into(), cap_gb: 500 }, Setting { dir: DIR.into(), cap_gb: 250 }] {
        run_batch(&cap_job(), &Mutex::new(now), deps, &mut low, &mut warner, &mut |_| {});
    }
    assert_eq!(RECYCLED_C.lock().unwrap().len(), 0);
    // The setting the job was made for still runs.
    run_batch(&cap_job(), &Mutex::new(Setting { dir: DIR.into(), cap_gb: 500 }), deps, &mut low, &mut warner, &mut |_| {});
    assert_eq!(RECYCLED_C.lock().unwrap().len(), 1);
}

#[test]
fn the_pump_publishes_the_current_setting() {
    let (mut j, _rx, t0) = janitor();
    j.tick(DIR, 500, BUFFER, t0);
    assert_eq!(*j.current().lock().unwrap(), Setting { dir: DIR.into(), cap_gb: 500 });
    j.run_now(r"G:\NewClips", 0, t0);
    assert_eq!(*j.current().lock().unwrap(), Setting { dir: r"G:\NewClips".into(), cap_gb: 0 });
}

fn no_keeps() -> Result<std::collections::HashSet<String>, String> {
    Ok(Default::default())
}

fn any_bin(_: &Path, _: u64) -> Result<(), String> {
    Ok(())
}

/// A volume without a Recycle Bin (a \\nas share, a USB stick).
fn no_bin(_: &Path, _: u64) -> Result<(), String> {
    Err("the drive has no Recycle Bin".into())
}

static RECYCLED_D: StdMutex<Vec<Instant>> = StdMutex::new(Vec::new());
fn recycle_d(_: &[PathBuf]) -> Result<(), crate::record_storage::RecycleError> {
    RECYCLED_D.lock().unwrap().push(Instant::now());
    Ok(())
}

#[test]
fn a_volume_without_a_recycle_bin_deletes_nothing_and_tells_once() {
    let deps = Deps { free: roomy, resolve: by_letter, plan: plan_one, bin: no_bin, recycle: recycle_d, keep: no_keeps, present: |_| true };
    let current = Mutex::new(Setting { dir: DIR.into(), cap_gb: 500 });
    let (mut low, mut warner) = (FreeWarner::default(), Warner::default());
    let mut events = Vec::new();
    for _ in 0..2 {
        run_batch(&cap_job(), &current, deps, &mut low, &mut warner, &mut |e| events.push(e.to_string()));
    }
    assert_eq!(RECYCLED_D.lock().unwrap().len(), 0, "nothing deleted or recycled");
    let notice = crate::record_status::error_event(crate::record_storage::REFUSED_NOTICE);
    assert_eq!(events.iter().filter(|e| **e == notice).count(), 1, "one toast across runs: {events:?}");
}

static RECYCLED_BUSY: StdMutex<Vec<Instant>> = StdMutex::new(Vec::new());
/// The Recycle Bin call refused because an earlier one is stuck.
fn recycle_busy(_: &[PathBuf]) -> Result<(), crate::record_storage::RecycleError> {
    RECYCLED_BUSY.lock().unwrap().push(Instant::now());
    Err(crate::record_storage::RecycleError::Refused(crate::win_fileop::STUCK_REFUSAL.into()))
}

#[test]
fn a_busy_recycle_bin_shows_the_busy_notice_once_per_streak() {
    let deps = Deps { free: roomy, resolve: by_letter, plan: plan_one, bin: any_bin, recycle: recycle_busy, keep: no_keeps, present: |_| true };
    let current = Mutex::new(Setting { dir: DIR.into(), cap_gb: 500 });
    let (mut low, mut warner) = (FreeWarner::default(), Warner::default());
    let mut events = Vec::new();
    for _ in 0..3 {
        run_batch(&cap_job(), &current, deps, &mut low, &mut warner, &mut |e| events.push(e.to_string()));
    }
    let busy = crate::record_status::error_event(crate::record_storage::BUSY_NOTICE);
    assert_eq!(events.iter().filter(|e| **e == busy).count(), 1, "one busy toast across runs: {events:?}");
    assert!(crate::record_storage::BUSY_NOTICE.contains("busy, try again"));
}

fn recycle_never(_: &[PathBuf]) -> Result<(), crate::record_storage::RecycleError> {
    panic!("a run for a missing clip folder recycled something")
}

/// The clip drive is unplugged (or the folder renamed) while other clips exist, say in the
/// C: preview cache: the cap run for that folder skips before it plans, so nothing anywhere is
/// moved and nothing is reported as used or cleaned.
#[test]
fn a_missing_clip_folder_skips_the_cap_run() {
    let deps = Deps { free: roomy, resolve: by_letter, plan: plan_one, bin: any_bin, recycle: recycle_never, keep: no_keeps, present: |_| false };
    let job = cap_job();
    let current = Mutex::new(Setting { dir: job.dir.clone(), cap_gb: job.cap_gb });
    let mut events = Vec::new();
    run_batch(&job, &current, deps, &mut FreeWarner::default(), &mut Warner::default(), &mut |e| events.push(e.to_string()));
    assert!(events.iter().all(|e| !e.contains("storage_cleaned") && !e.contains("storage_warning")), "{events:?}");
}

/// The production check: a folder that is there counts, a missing one (or a missing drive) does not.
#[test]
fn folder_present_sees_missing_folders() {
    assert!(folder_present(&std::env::temp_dir()));
    let gone = std::env::temp_dir().join(format!("recall-janitor-missing-{}", std::process::id()));
    assert!(!folder_present(&gone));
    assert!(!folder_present(&gone.join("Game")));
}
