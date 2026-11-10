//! Restore policy review (P2-1, P2-2, P2-5, P2-7, P2-8, P3), long-recording side, fail-first:
//! the in-progress list is never overwritten from a failed read, a part on a missing drive
//! stays listed, an entry that is not a long recording is never acted on, the one-time scan
//! never recycles, a part that could not be listed asks for the scan again, a failed undo keeps
//! its record, and Resume of a renamed recovery continues the recording's own name. Temp folders,
//! a temp app-data root, the fake Recycle Bin.

use super::restore_tests::{crashed_recording, header_only, sandbox};
use super::*;
use crate::encoder::in_progress::{self, Entry, Undo};
use crate::encoder::recycle::fake;
use std::os::windows::fs::OpenOptionsExt;
use test_sink::{pkt, tracks, FakeFactory, MS};

const NAME: &str = "G__2026-11-11__09-00-00__rec.mp4";

/// P2-1: the list that cannot be read (here: open elsewhere without read sharing) is never
/// replaced by one that lacks what it held.
#[test]
fn a_list_that_cannot_be_read_is_never_overwritten() {
    let sb = sandbox("review-unreadable");
    let first = sb.dir.join(format!("{NAME}.part"));
    in_progress::add(Entry::long(&first)).unwrap();
    let list = in_progress::path().unwrap();
    // Delete sharing only: a read fails, a rename over it would not.
    let held = std::fs::OpenOptions::new().read(true).share_mode(0x4).open(&list).unwrap();
    let added = in_progress::add(Entry::long(&sb.dir.join("H__2026-11-11__09-05-00__rec.mp4.part")));
    drop(held);
    assert!(in_progress::get(&first).is_some(), "the entry already listed survives (the add gave {added:?})");
    assert!(added.is_err(), "the add that could not read the list fails");
}

/// P2-2: a listed part whose folder is gone (its drive not mounted yet) stays listed.
#[test]
fn a_listed_part_on_a_missing_drive_stays_listed() {
    let sb = sandbox("review-missing-drive");
    let part = sb.dir.join("not mounted").join(format!("{NAME}.part"));
    in_progress::add(Entry::long(&part)).unwrap();
    let found = recover::run(&in_progress::list(), &sb.dir, None);
    assert!(in_progress::get(&part).is_some(), "kept for when its drive is back: {:?}", found.logs);
}

/// P2-5: an entry that is not a long recording's part, or whose undo record points at another
/// file, is logged and dropped from the list, never acted on.
#[test]
fn an_entry_that_is_not_a_long_recording_is_never_touched() {
    let sb = sandbox("review-invalid");
    in_progress::mark_legacy_scanned().unwrap(); // only the list's entries are under test here
    let notes = sb.dir.join("notes.txt");
    std::fs::write(&notes, b"the user's notes").unwrap();
    in_progress::add(Entry::long(&notes)).unwrap();
    let part = sb.dir.join(format!("{NAME}.part"));
    std::fs::write(&part, b"fragments").unwrap();
    let precious = sb.dir.join("precious.bin");
    std::fs::write(&precious, b"someone else's file").unwrap();
    let undo = Undo { len: 9 + 19, keep: 9, mvhd_at: 0, mvhd_old: String::new(), tail: precious.to_string_lossy().into_owned(), tail_hash: in_progress::fnv_hex(b"someone else's file") };
    in_progress::add(Entry { undo: Some(undo), ..Entry::long(&part) }).unwrap();
    let found = recover::run(&in_progress::list(), &sb.dir, None);
    assert_eq!(std::fs::read(&notes).unwrap(), b"the user's notes", "not renamed, not recycled: {:?}", found.logs);
    assert!(!sb.dir.join("notes-recovered.mp4").exists());
    assert!(precious.is_file() && std::fs::read(&part).unwrap() == b"fragments", "the undo record's foreign tail is never used: {:?}", found.logs);
    assert!(fake::binned_under(&sb.dir).is_empty());
    assert!(in_progress::get(&notes).is_none() && in_progress::get(&part).is_none(), "both dropped from the list");
    assert!(found.logs.iter().filter(|l| l.contains("not acted on")).count() == 2, "{:?}", found.logs);
}

/// P2-7: the one-time scan never recycles, also a part with nothing playable in it.
#[test]
fn the_legacy_scan_keeps_a_part_with_nothing_playable() {
    let sb = sandbox("review-legacy-keep");
    let Some(bytes) = header_only(&sb.dir) else { return eprintln!("libx264 missing; skipped") };
    let part = sb.dir.join(format!("{NAME}.part"));
    std::fs::write(&part, &bytes).unwrap();
    let mut found = recover::Report::default();
    recover::legacy_scan(&sb.dir, None, &mut found);
    assert!(part.is_file() && fake::binned_under(&sb.dir).is_empty(), "kept: {:?}", found.logs);
    assert!(found.logs.iter().any(|l| l.contains("kept")), "and logged: {:?}", found.logs);
}

/// P2-8: a part that could not be listed asks for the one-time scan again, so a crash never
/// leaves it to nobody.
#[test]
fn a_part_that_cannot_be_listed_asks_for_the_scan_again() {
    let sb = sandbox("review-unlisted");
    in_progress::mark_legacy_scanned().unwrap();
    in_progress::add(Entry::long(&sb.dir.join("other.mp4.part"))).unwrap();
    let held = std::fs::OpenOptions::new().read(true).share_mode(0).open(in_progress::path().unwrap()).unwrap();
    let mut rec = machine::Recorder::new(FakeFactory::default(), tracks());
    let t0 = Instant::now();
    rec.start(sb.dir.join(NAME), Some((0, t0)), t0);
    let events = rec.video(100 * MS, true, pkt(1), false, t0);
    drop(held);
    assert!(events.iter().any(|e| matches!(e, Event::Log(l) if l.contains("not listed"))), "{events:?}");
    assert!(!in_progress::legacy_scanned(), "the next start scans for it again");
    rec.stop();
}

/// P3: an undo that cannot run (its saved tail changed) keeps its record and its tail.
#[test]
fn a_failed_undo_keeps_its_record_and_tail() {
    let sb = sandbox("review-undo-kept");
    let Some(bytes) = crashed_recording(&sb.dir) else { return eprintln!("libx264 missing; skipped") };
    let part = sb.dir.join(format!("{NAME}.part"));
    std::fs::write(&part, &bytes[..bytes.len() * 77 / 100]).unwrap();
    let hooks = restore::Hooks { stop_after_patch: true, ..Default::default() };
    assert_eq!(restore::repair_with(&part, hooks), restore::Repair::Interrupted);
    let tail = restore::tail_path(&part);
    std::fs::OpenOptions::new().append(true).open(&tail).unwrap().write_all(b"x").unwrap();
    assert!(restore::undo(&part).is_err());
    assert!(in_progress::get(&part).and_then(|e| e.undo).is_some(), "the record stays");
    assert!(tail.is_file(), "and the tail beside it");
}

/// P3: Resume of a recovery renamed `-recovered` (the name was taken) continues the recording's
/// own name, which recovery recognises as a long recording.
#[test]
fn resume_of_a_renamed_recovery_continues_the_recording_name() {
    let sb = sandbox("review-resume-name");
    let recovered = sb.dir.join("G__2026-11-11__09-00-00__rec-recovered.mp4");
    std::fs::write(&recovered, b"recovered").unwrap();
    let next = recover::next_part_of(&recovered, &mut Vec::new()).expect("continues");
    assert_eq!(next, sb.dir.join("G__2026-11-11__09-00-00__rec-2.mp4"));
    assert!(naming::is_longrec_part(&format!("{}.part", next.file_name().unwrap().to_string_lossy())));
}

use std::io::Write as _;
