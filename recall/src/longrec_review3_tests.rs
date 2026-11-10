//! Restore policy review round 3, long-recording side, fail-first: bookmarks never land on a
//! different recording that took the name, and a kept part with nothing whole in it asks the
//! next start to scan again. Temp folders, a temp app-data root, the fake Recycle Bin.

use super::restore_tests::{crashed_recording, last_whole_fragment_end, sandbox};
use super::*;
use crate::encoder::in_progress::{self, Entry};

const NAME: &str = "G__2026-11-11__13-00-00__rec.mp4";

/// The part is gone and its name belongs to another recording (this one was recovered as
/// `-recovered`, with its bookmarks): the other recording never gets this one's bookmarks.
#[test]
fn bookmarks_never_land_on_the_recording_that_took_the_name() {
    let sb = sandbox("r3-marks-taken");
    let (part, other, ours) = (sb.dir.join(format!("{NAME}.part")), sb.dir.join(NAME), sb.dir.join("G__2026-11-11__13-00-00__rec-recovered.mp4"));
    std::fs::write(&other, b"another recording").unwrap();
    std::fs::write(&ours, b"this recording").unwrap();
    in_progress::add(Entry { base_us: Some(100 * 1000), marks: vec![600 * 1000], ..Entry::long(&part) }).unwrap();
    let found = recover::run(&in_progress::list(), &sb.dir, None);
    assert!(!crate::bookmarks::sidecar_path(&other).exists(), "{:?}", found.logs);
    assert!(in_progress::list().is_empty());
}

/// A listed part with media in it but no whole fragment is kept (never recycled) and the next
/// start's scan looks at it again.
#[test]
fn a_kept_part_with_nothing_whole_asks_for_the_scan_again() {
    let sb = sandbox("r3-partial-first");
    let Some(bytes) = crashed_recording(&sb.dir) else { return eprintln!("libx264 missing; skipped") };
    let header = bytes.windows(4).position(|w| w == b"moof").unwrap() - 4;
    assert!(last_whole_fragment_end(&bytes[..header + 300]) <= header, "no whole fragment in the fixture");
    let part = sb.dir.join(format!("{NAME}.part"));
    std::fs::write(&part, &bytes[..header + 300]).unwrap();
    in_progress::mark_legacy_scanned().unwrap();
    in_progress::add(Entry::long(&part)).unwrap();
    let found = recover::run(&in_progress::list(), &sb.dir, None);
    assert!(part.is_file(), "kept: {:?}", found.logs);
    assert!(!in_progress::legacy_scanned(), "the next start scans again: {:?}", found.logs);
}
