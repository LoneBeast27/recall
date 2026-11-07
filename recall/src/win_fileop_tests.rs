//! The sink's decision, the flag set and the outcome mapping (pure). The live
//! probe is in `win_fileop_probe.rs` (ignored by default; run on purpose).

use super::*;

#[test]
fn the_sink_lets_a_delete_through_only_when_it_recycles() {
    assert!(allows_delete(TSF_DELETE_RECYCLE_IF_POSSIBLE, false));
    assert!(allows_delete(TSF_DELETE_RECYCLE_IF_POSSIBLE | 0x202, false), "other bits do not matter");
    assert!(!allows_delete(0x202, false), "no recycle bit: a permanent delete, refused");
    assert!(!allows_delete(0, false));
    assert!(!allows_delete(TSF_DELETE_RECYCLE_IF_POSSIBLE, true), "after the caller stopped waiting, nothing goes");
}

#[test]
fn the_operation_flags_are_exactly_recycle_silent_and_the_nuke_warning() {
    // FOFX_RECYCLEONDELETE | FOF_ALLOWUNDO | FOF_NOCONFIRMATION | FOF_SILENT | FOF_NOERRORUI | FOF_WANTNUKEWARNING
    assert_eq!(OP_FLAGS, 0x80000 | 0x40 | 0x10 | 0x4 | 0x400 | 0x4000);
    assert_ne!(OP_FLAGS & FOF_WANTNUKEWARNING.0, 0, "the backstop: Windows asks rather than deletes silently");
}

#[test]
fn outcomes_from_the_sinks_the_count_and_the_operation() {
    assert_eq!(classify(false, (2, 2), Ok(()), false), Outcome::Recycled);
    assert!(matches!(classify(true, (1, 2), Err(E_ABORT.0 as u32), true), Outcome::Refused(_)), "the sink refused");
    assert!(matches!(classify(false, (2, 2), Err(0x8007_04C7), false), Outcome::Refused(_)), "ERROR_CANCELLED");
    assert!(matches!(classify(false, (2, 2), Err(0x8027_0000), false), Outcome::Refused(_)), "copy engine cancelled");
    assert!(matches!(classify(false, (2, 2), Ok(()), true), Outcome::Refused(_)), "aborted");
    assert!(matches!(classify(false, (2, 2), Err(0x8007_0005), false), Outcome::Failed(_)), "access denied is a failure");
    let unchecked = classify(false, (1, 2), Ok(()), false);
    assert!(matches!(&unchecked, Outcome::Refused(why) if why.contains("1 of 2 items were not checked")), "{unchecked:?}");
}

static TEST_STUCK: AtomicBool = AtomicBool::new(true);
static TEST_SPAWNED: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

#[test]
fn a_call_while_an_earlier_one_is_stuck_is_refused_and_starts_no_thread() {
    // A path that does not exist: nothing could be deleted even if a worker ran.
    let ghost = std::env::temp_dir().join("recall-fileop-never-exists").join("ghost.txt");
    let report = recycle_with_slot(std::slice::from_ref(&ghost), WAIT, OP_FLAGS, &TEST_STUCK, &TEST_SPAWNED, REAL_DESKTOP);
    assert_eq!(report.outcome, Outcome::Refused(STUCK_REFUSAL.into()));
    assert_eq!(TEST_SPAWNED.load(Ordering::SeqCst), 0, "no new STA thread");
}

static RELEASE_STUCK: AtomicBool = AtomicBool::new(false);
static RELEASE_SPAWNED: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

#[test]
fn the_stuck_slot_is_taken_on_a_timeout_and_released_when_that_thread_ends() {
    let ghost = std::env::temp_dir().join("recall-fileop-never-exists").join("ghost.txt");
    // A zero wait abandons the call at once; the worker then fails on the missing path and ends.
    let report = recycle_with_slot(std::slice::from_ref(&ghost), Duration::ZERO, OP_FLAGS, &RELEASE_STUCK, &RELEASE_SPAWNED, REAL_DESKTOP);
    assert!(matches!(&report.outcome, Outcome::Refused(why) if why.contains("did not finish")), "{report:?}");
    let deadline = std::time::Instant::now() + Duration::from_secs(20);
    while RELEASE_STUCK.load(Ordering::SeqCst) && std::time::Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!(!RELEASE_STUCK.load(Ordering::SeqCst), "released when the stuck thread ended");
    assert_eq!(RELEASE_SPAWNED.load(Ordering::SeqCst), 1);
}

// ---- The private desktop: no desktop, no call ----

static DESK_STUCK: AtomicBool = AtomicBool::new(false);
static DESK_SPAWNED: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
static CLOSED: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

fn no_desktop(_: &str) -> Option<isize> {
    None
}
fn fake_desktop(_: &str) -> Option<isize> {
    Some(42)
}
fn cannot_set(_: isize) -> bool {
    false
}
fn count_close(desk: isize) {
    assert_eq!(desk, 42);
    CLOSED.fetch_add(1, Ordering::SeqCst);
}

#[test]
fn without_a_private_desktop_the_call_is_refused_and_nothing_is_touched() {
    // A path that does not exist: even a call that went ahead could delete nothing.
    let ghost = std::env::temp_dir().join("recall-fileop-never-exists").join("ghost.txt");
    let none = DesktopFns { create: no_desktop, set: cannot_set, close: count_close };
    let report = recycle_with_slot(std::slice::from_ref(&ghost), WAIT, OP_FLAGS, &DESK_STUCK, &DESK_SPAWNED, none);
    assert!(matches!(&report.outcome, Outcome::Refused(why) if why.contains("no private desktop")), "{report:?}");
    assert_eq!(report.flags_seen, Vec::<u32>::new(), "no shell call");

    let unset = DesktopFns { create: fake_desktop, set: cannot_set, close: count_close };
    let report = recycle_with_slot(std::slice::from_ref(&ghost), WAIT, OP_FLAGS, &DESK_STUCK, &DESK_SPAWNED, unset);
    assert!(matches!(&report.outcome, Outcome::Refused(why) if why.contains("private desktop")), "{report:?}");
    assert_eq!(CLOSED.load(Ordering::SeqCst), 1, "the desktop it made is closed after the worker ended");
}

#[path = "win_fileop_probe.rs"]
mod probe;
