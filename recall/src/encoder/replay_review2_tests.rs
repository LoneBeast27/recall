//! Restore policy review round 2 (P2-A, P2-B, the recycle invariant, P3s), replay side,
//! fail-first: a `.part` is recycled only when a scan of the `.part` itself finds no media; an
//! index that lost its records never costs the save; a zero hole skips only its own packets; a
//! refused index is kept once, not retried forever; an unreadable list at start keeps the first
//! save off the unclaimed slot; the real Recycle Bin precheck says yes on a temp file. Real
//! libx264 + AAC through the production mux (CPU only), temp folders and app-data root, fake bin.

use std::path::{Path, PathBuf};

use super::super::in_progress::{self, Entry};
use super::super::recycle::{self, dispose_unrecoverable, fake};
use super::super::replay_index::{self, claim, idx_of, slot};
use super::replay_review_tests::{listed_cut, moov_at, sandbox, save_into, truncate, video_packets};
use super::*;

const HEADER: [u8; 16] = [0, 0, 0, 16, b'f', b't', b'y', b'p', b'i', b's', b'o', b'm', 0, 0, 2, 0];

/// The invariant: a `.part` goes to the Recycle Bin only when the `.part` itself holds no media
/// (its `mdat` empty or all zeros); one with data in it is kept, with one diagnostic line.
#[test]
fn a_part_is_recycled_only_when_it_holds_no_media() {
    let sb = sandbox("invariant");
    let mdat = |payload: &[u8]| [&HEADER[..], &((8 + payload.len()) as u32).to_be_bytes(), b"mdat", payload].concat();
    let media = sb.dir.join("A__2026-11-11__11-00-00-000__aa.mp4.part");
    std::fs::write(&media, mdat(&[0, 0, 0, 5, 0x65, 1, 2, 3, 4])).unwrap();
    let lines = dispose_unrecoverable(&media, "test");
    assert!(media.exists(), "data in its mdat: kept: {lines:?}");
    assert_eq!(lines.len(), 1, "one diagnostic line: {lines:?}");
    let (empty, zeros) = (sb.dir.join("B__2026-11-11__11-00-00-000__bb.mp4.part"), sb.dir.join("C__2026-11-11__11-00-00-000__cc.mp4.part"));
    std::fs::write(&empty, mdat(&[])).unwrap();
    std::fs::write(&zeros, mdat(&[0; 4096])).unwrap();
    dispose_unrecoverable(&empty, "test");
    dispose_unrecoverable(&zeros, "test");
    assert!(!empty.exists() && !zeros.exists(), "nothing but headers and zeros: to the Recycle Bin");
}

/// P2-A: after a power cut the index head was on disk but none of its records: the `.part`
/// full of packets is kept, never recycled as "no video".
#[test]
fn a_part_whose_index_lost_its_records_is_kept() {
    let sb = sandbox("no-records");
    let Some((part, _, entry)) = listed_cut(&sb, "D__2026-11-11__11-00-00-000__dd", 0.95) else { return eprintln!("libx264 missing; skipped") };
    let idx = idx_of(Path::new(entry.index.as_deref().unwrap()));
    let bytes = std::fs::read(&idx).unwrap();
    let head = 12 + u32::from_le_bytes(bytes[8..12].try_into().unwrap()) as usize;
    std::fs::write(&idx, &bytes[..head]).unwrap();
    let (mut clips, mut logs) = (Vec::new(), Vec::new());
    recover_entry(&entry, &mut clips, &mut logs);
    assert!(part.exists() && !fake::binned_under(&sb.dir).contains(&part), "the packets in the .part stay: {logs:?}");
}

/// P2-B: an out-of-order flush left a hole of zeros in the middle: only the packets in the hole
/// are skipped; every whole packet after it is in the clip.
#[test]
fn a_zero_hole_skips_only_its_own_packets() {
    let sb = sandbox("zero-hole");
    let Some((part, clip, entry)) = listed_cut(&sb, "E__2026-11-11__11-00-00-000__ee", 1.0) else { return eprintln!("libx264 missing; skipped") };
    let (_, records) = replay_index::read(Path::new(entry.index.as_deref().unwrap())).unwrap();
    let total = records.iter().filter(|r| r.stream == 0).count();
    let hole = records.iter().filter(|r| r.stream == 0).nth(total / 3).copied().unwrap();
    let mut bytes = std::fs::read(&part).unwrap();
    bytes[hole.offset as usize..(hole.offset + u64::from(hole.size)) as usize].fill(0);
    std::fs::write(&part, &bytes).unwrap();
    let (mut clips, mut logs) = (Vec::new(), Vec::new());
    recover_entry(&entry, &mut clips, &mut logs);
    assert_eq!(clips, vec![clip.clone()], "{logs:?}");
    assert_eq!(video_packets(&clip), total - 1, "all but the one packet in the hole");
}

/// P3: an index that belongs to another save is refused once: the part kept, logged, unlisted.
#[test]
fn a_refused_index_is_kept_once_not_retried() {
    let sb = sandbox("refused");
    let theirs = sb.dir.join("F__2026-11-11__11-00-00-000__ff.mp4.part");
    let Some(()) = save_into(&theirs, 30) else { return eprintln!("libx264 missing; skipped") };
    let index = claim(&slot().unwrap()).unwrap();
    let mine = sb.dir.join("G__2026-11-11__11-00-00-000__gg.mp4.part");
    std::fs::copy(&theirs, &mine).unwrap();
    truncate(&mine, moov_at(&mine));
    let entry = Entry::replay(&mine, &mine.with_extension(""), Some(&index));
    in_progress::add(entry.clone()).unwrap();
    let (mut clips, mut logs) = (Vec::new(), Vec::new());
    recover_entry(&entry, &mut clips, &mut logs);
    assert!(mine.exists() && clips.is_empty(), "{logs:?}");
    assert!(in_progress::get(&mine).is_none(), "not retried at every start: {logs:?}");
    assert!(idx_of(&index).exists(), "the other save's index is not this one's to recycle");
}

/// P3: when the list cannot be read at start, the saves of this run write their index to a
/// slot of their own, never over the slot an unclaimed save may still need.
#[test]
fn an_unreadable_list_at_start_keeps_saves_off_the_slot() {
    use std::os::windows::fs::OpenOptionsExt;
    let sb = sandbox("unreadable-start");
    let before = slot().unwrap();
    in_progress::add(Entry::replay(&sb.dir.join("H__2026-11-11__11-00-00-000__hh.mp4.part"), &sb.dir.join("H.mp4"), Some(&before))).unwrap();
    let held = std::fs::OpenOptions::new().read(true).share_mode(0).open(in_progress::path().unwrap()).unwrap();
    let mut logs = Vec::new();
    let entries = crate::longrec::recover::start_entries(&mut logs);
    drop(held);
    assert!(entries.is_empty());
    let after = slot();
    replay_index::tests_reset_run_slot();
    assert_ne!(after, Some(before), "saves of this run use a slot of their own: {logs:?}");
}

/// The real precheck on a temp file: its volume has a Recycle Bin that takes it.
#[test]
fn the_real_recycle_bin_precheck_accepts_a_temp_file() {
    let sb = sandbox("precheck-yes");
    let file: PathBuf = sb.dir.join("small.bin");
    std::fs::write(&file, b"x").unwrap();
    assert_eq!(recycle::precheck(&file), Ok(()), "%TEMP% is on a volume with a Recycle Bin");
    assert!(file.exists(), "the precheck moves nothing");
}

/// Review 3: a replay save that cannot be listed asks the next start to scan again (the scan
/// then finds it if this run crashes before its rename).
#[test]
fn a_save_that_cannot_be_listed_asks_for_the_scan_again() {
    use std::os::windows::fs::OpenOptionsExt;
    let sb = sandbox("unlisted-save");
    let Some((packets, params)) = super::super::replay_index::tests::window(10) else { return eprintln!("libx264 missing; skipped") };
    in_progress::mark_legacy_scanned().unwrap();
    in_progress::add(Entry::replay(&sb.dir.join("other.mp4.part"), &sb.dir.join("other.mp4"), None)).unwrap();
    let held = std::fs::OpenOptions::new().read(true).share_mode(0).open(in_progress::path().unwrap()).unwrap();
    let clip = sb.dir.join("I__2026-11-11__11-00-00-000__ii.mp4");
    let secs = super::super::saver::window_secs(&packets);
    let job = super::super::mux::tests::window_job(&clip.to_string_lossy(), packets, params, secs);
    let part = crate::naming::part_of(&clip);
    let mut timing = super::super::saver::Timing::default();
    let lines = std::cell::RefCell::new(Vec::new());
    let saved = super::super::saver::write_clip_to(&part, &clip, &job, &mut super::super::mux::produce, &std::sync::atomic::AtomicBool::new(false), &mut timing, &|l: &str| lines.borrow_mut().push(l.to_string()));
    drop(held);
    assert!(saved.is_ok() && clip.is_file(), "the save itself goes on: {:?}", lines.borrow());
    assert!(!in_progress::legacy_scanned(), "the next start scans again: {:?}", lines.borrow());
}

/// Review 3: a run that used its own slot recycles it at a clean quit (its own temp files).
#[test]
fn a_clean_quit_recycles_the_runs_own_slot() {
    let _sb = sandbox("run-slot");
    replay_index::use_run_slot();
    let base = slot().unwrap();
    std::fs::create_dir_all(base.parent().unwrap()).unwrap();
    for f in [idx_of(&base), replay_index::hdr_of(&base)] {
        std::fs::write(&f, b"x").unwrap();
    }
    replay_index::drop_run_slot();
    let gone = !idx_of(&base).exists() && !replay_index::hdr_of(&base).exists();
    replay_index::tests_reset_run_slot();
    assert!(gone, "the run's own index files went to the Recycle Bin");
}
