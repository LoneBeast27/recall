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

use std::sync::atomic::AtomicU64;

/// A path that does not exist: even a call that went ahead could delete nothing.
fn ghost() -> PathBuf {
    std::env::temp_dir().join("recall-fileop-never-exists").join("ghost.txt")
}

static TEST_STUCK: AtomicU64 = AtomicU64::new(99);
static TEST_SPAWNED: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

#[test]
fn a_call_while_the_slot_is_held_gets_the_busy_notice_and_starts_no_thread() {
    let report = recycle_with_slot(&[ghost()], WAIT, OP_FLAGS, &TEST_STUCK, &TEST_SPAWNED, REAL_DESKTOP);
    assert_eq!(report.outcome, Outcome::Refused(STUCK_REFUSAL.into()));
    assert!(STUCK_REFUSAL.contains("busy") && STUCK_REFUSAL.contains("try again"), "{STUCK_REFUSAL}");
    assert_eq!(TEST_SPAWNED.load(Ordering::SeqCst), 0, "no new STA thread");
}

static RELEASE_STUCK: AtomicU64 = AtomicU64::new(0);
static RELEASE_SPAWNED: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

#[test]
fn a_stuck_call_that_returns_frees_the_slot_and_the_next_call_proceeds() {
    // A zero wait abandons the call at once; the worker then fails on the missing path and ends.
    let report = recycle_with_slot(&[ghost()], Duration::ZERO, OP_FLAGS, &RELEASE_STUCK, &RELEASE_SPAWNED, REAL_DESKTOP);
    assert!(matches!(&report.outcome, Outcome::Refused(why) if why.contains("did not finish")), "{report:?}");
    let deadline = std::time::Instant::now() + Duration::from_secs(20);
    while RELEASE_STUCK.load(Ordering::SeqCst) != 0 && std::time::Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(50));
    }
    assert_eq!(RELEASE_STUCK.load(Ordering::SeqCst), 0, "released when the stuck thread ended");
    // No app restart: the next call goes ahead (and fails only on the missing path).
    let next = recycle_with_slot(&[ghost()], WAIT, OP_FLAGS, &RELEASE_STUCK, &RELEASE_SPAWNED, REAL_DESKTOP);
    assert!(matches!(&next.outcome, Outcome::Failed(why) if why.contains("cannot find")), "{next:?}");
    assert_eq!(RELEASE_SPAWNED.load(Ordering::SeqCst), 2);
}

#[test]
fn an_old_token_cannot_release_a_newer_holder() {
    let slot = AtomicU64::new(7);
    assert!(!release_slot(&slot, 3), "an older call's thread");
    assert_eq!(slot.load(Ordering::SeqCst), 7);
    assert!(!release_slot(&slot, 0), "no token, no release");
    assert!(release_slot(&slot, 7));
    assert_eq!(slot.load(Ordering::SeqCst), 0);
}

// ---- The private desktop: no access list or no desktop, no call ----

static DESK_STUCK: AtomicU64 = AtomicU64::new(0);
static DESK_SPAWNED: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
static CLOSED: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

fn no_dacl() -> Option<String> {
    None
}
fn some_dacl() -> Option<String> {
    Some("D:P(A;;0x87;;;S-1-5-21-1-2-3-1001)".into())
}
fn no_desktop(_: &str, _: &str) -> Option<isize> {
    None
}
fn fake_desktop(_: &str, sddl: &str) -> Option<isize> {
    assert!(sddl.starts_with("D:P(A;;"), "always an explicit access list: {sddl}");
    Some(42)
}
fn cannot_set(_: isize) -> bool {
    false
}
fn count_close(desk: isize) {
    assert_eq!(desk, 42);
    CLOSED.fetch_add(1, Ordering::SeqCst);
}

static MADE_WITHOUT_DACL: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
fn count_made(_: &str, _: &str) -> Option<isize> {
    MADE_WITHOUT_DACL.fetch_add(1, Ordering::SeqCst);
    None
}

#[test]
fn without_an_access_list_the_call_is_refused_and_nothing_is_made() {
    let none = DesktopFns { dacl: no_dacl, create: count_made, set: cannot_set, close: count_close };
    let report = recycle_with_slot(&[ghost()], WAIT, OP_FLAGS, &DESK_STUCK, &DESK_SPAWNED, none);
    assert!(matches!(&report.outcome, Outcome::Refused(why) if why.contains("access list")), "{report:?}");
    assert_eq!(report.flags_seen, Vec::<u32>::new(), "no shell call");
    assert_eq!(MADE_WITHOUT_DACL.load(Ordering::SeqCst), 0, "no desktop made");
}

#[test]
fn without_a_private_desktop_the_call_is_refused_and_nothing_is_touched() {
    let none = DesktopFns { dacl: some_dacl, create: no_desktop, set: cannot_set, close: count_close };
    let report = recycle_with_slot(&[ghost()], WAIT, OP_FLAGS, &DESK_STUCK, &DESK_SPAWNED, none);
    assert!(matches!(&report.outcome, Outcome::Refused(why) if why.contains("no private desktop")), "{report:?}");
    assert_eq!(report.flags_seen, Vec::<u32>::new(), "no shell call");

    let unset = DesktopFns { dacl: some_dacl, create: fake_desktop, set: cannot_set, close: count_close };
    let closed = CLOSED.load(Ordering::SeqCst);
    let report = recycle_with_slot(&[ghost()], WAIT, OP_FLAGS, &DESK_STUCK, &DESK_SPAWNED, unset);
    assert!(matches!(&report.outcome, Outcome::Refused(why) if why.contains("private desktop")), "{report:?}");
    assert_eq!(CLOSED.load(Ordering::SeqCst), closed + 1, "the desktop it made is closed after the worker ended");
}

#[test]
fn the_access_list_is_one_ace_for_this_user_with_the_desktop_rights() {
    let sid = dacl::token_user_sid().expect("token user");
    assert!(sid.starts_with("S-1-5-"), "{sid}");
    assert_eq!(dacl::own_sddl(), Some(format!("D:P(A;;0x87;;;{sid})")));
}

#[path = "win_fileop_probe.rs"]
mod probe;
