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

#[path = "win_fileop_probe.rs"]
mod probe;
