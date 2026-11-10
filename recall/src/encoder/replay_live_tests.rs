//! Live checks on d835afda (`.tmp/live/LIVE-CHECKS.md`), replay side, fail-first: an index
//! record reaches Windows with its packet (a process kill loses nothing the `.part` holds), a
//! `.part` that holds more than its rebuild stays listed (never orphaned, never retried for
//! nothing), and recycling the cut `.part` after a rebuild is logged. Real libx264 + AAC through
//! the production mux (CPU only), temp folders and app-data root, the fake Recycle Bin.

use std::path::Path;

use super::super::in_progress;
use super::super::recycle::fake;
use super::super::replay_index::{self, idx_of, Record};
use super::replay_review_tests::{listed_cut, sandbox, video_packets};
use super::*;

/// A record written through is in the file as soon as it is pushed (no buffer a kill loses).
#[test]
fn every_index_record_reaches_windows_as_it_is_pushed() {
    let sb = sandbox("write-through");
    let base = sb.dir.join("save");
    let mut w = replay_index::begin(&base, &sb.dir.join("A.mp4.part"), |hdr: &Path| std::fs::write(hdr, b"hdr").map_err(|e| e.to_string()), &[(1, 1_000_000)], None).unwrap();
    let head = std::fs::metadata(idx_of(&base)).unwrap().len();
    for i in 0..3 {
        w.push(i as usize, Record { stream: 1, key: false, size: 10, offset: 48 + i * 10, pts: i as i64, dur: 1 });
    }
    let on_disk = std::fs::metadata(idx_of(&base)).unwrap().len();
    assert_eq!(on_disk, head + 3 * 32, "three records in the file before any finish");
    drop(w);
}

/// The index lost its last records (a power cut): the `.part` holds more than the rebuild took.
/// It stays listed (never an orphan); a later start with the same index leaves it alone
/// without a log line; once the index has grown it is rebuilt again.
#[test]
fn a_part_that_holds_more_stays_listed_and_is_retried_only_for_a_grown_index() {
    let sb = sandbox("holds-more-listed");
    let Some((part, clip, entry)) = listed_cut(&sb, "B__2026-11-11__14-00-00-000__bb", 0.95) else { return eprintln!("libx264 missing; skipped") };
    let idx = idx_of(Path::new(entry.index.as_deref().unwrap()));
    let full = std::fs::read(&idx).unwrap();
    let head = 12 + u32::from_le_bytes(full[8..12].try_into().unwrap()) as usize;
    let records = (full.len() - head) / 32;
    std::fs::write(&idx, &full[..head + records / 2 * 32]).unwrap();
    let (mut clips, mut logs) = (Vec::new(), Vec::new());
    recover_entry(&entry, &mut clips, &mut logs);
    assert_eq!(clips, vec![clip.clone()], "{logs:?}");
    let listed = in_progress::get(&part).expect("kept listed: never an orphan");
    assert!(part.exists() && listed.held_more.is_some(), "{logs:?}");
    let (mut again, mut quiet) = (Vec::new(), Vec::new());
    recover_entry(&listed, &mut again, &mut quiet);
    assert!(again.is_empty() && quiet.is_empty(), "the same index: left alone, no log: {quiet:?}");
    std::fs::write(&idx, &full).unwrap();
    let before = video_packets(&clip);
    let (mut grown, mut logs) = (Vec::new(), Vec::new());
    recover_entry(&in_progress::get(&part).unwrap(), &mut grown, &mut logs);
    assert_eq!(grown, vec![clip.clone()], "rebuilt again over its own clip: {logs:?}");
    assert!(video_packets(&clip) > before, "with the packets the grown index knows");
}

/// Recycling the cut `.part` after a good rebuild says so: path, size, the clip, its length.
#[test]
fn recycling_the_cut_part_after_a_rebuild_is_logged() {
    let sb = sandbox("rebuild-log");
    let Some((part, clip, entry)) = listed_cut(&sb, "C__2026-11-11__14-00-00-000__cc", 0.7) else { return eprintln!("libx264 missing; skipped") };
    let size = std::fs::metadata(&part).unwrap().len();
    let (mut clips, mut logs) = (Vec::new(), Vec::new());
    recover_entry(&entry, &mut clips, &mut logs);
    assert!(fake::binned_under(&sb.dir).contains(&part), "{logs:?}");
    let line = logs.iter().find(|l| l.contains("Recycle Bin") && l.contains(&part.to_string_lossy().into_owned()));
    assert!(line.is_some_and(|l| l.contains(&format!("{size} B")) && l.contains(&clip.to_string_lossy().into_owned()) && l.contains(" s")), "{logs:?}");
}

// The live kill itself (`.tmp/live`) is `plan_tests::a_save_killed_mid_mux_loses_no_packet_the_part_holds`:
// a real mux killed mid-save, its buffers lost as a process kill loses them.
