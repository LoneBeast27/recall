//! Live checks on d835afda, long-recording side, fail-first: a repair that cuts nothing (only
//! the length is set) makes no `.tail` file, so nothing goes to the Recycle Bin.

use super::restore_tests::{crashed_recording, sandbox};
use super::*;

/// The live recording-kill: whole fragments only, the length unset. No `.tail` is made and
/// nothing is recycled.
#[test]
fn a_repair_that_cuts_nothing_makes_no_tail() {
    let sb = sandbox("live-no-tail");
    let Some(bytes) = crashed_recording(&sb.dir) else { return eprintln!("libx264 missing; skipped") };
    let part = sb.dir.join("G__2026-11-11__16-00-00__rec.mp4.part");
    std::fs::write(&part, &bytes).unwrap();
    assert!(matches!(restore::repair(&part), restore::Repair::Repaired { .. }));
    assert!(!restore::tail_path(&part).exists());
    assert!(crate::encoder::recycle::fake::binned_under(&sb.dir).is_empty(), "nothing to the Recycle Bin");
}
