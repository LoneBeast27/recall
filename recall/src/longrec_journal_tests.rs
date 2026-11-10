//! Restore policy, fail-first (`.planning/RESTORE-POLICY.md` §6 J1-J3, U1, P2, (e)): the
//! in-progress list a long recording is written under, recovery driven by it (wherever the
//! file is), the one-time scan for older builds' parts, an empty part's disposal, Resume
//! recording, and that recovery alone never starts a recording. Fake sink, temp folders, a temp
//! app-data root, the fake Recycle Bin.

use super::restore_tests::sandbox;
use super::*;
use crate::encoder::in_progress::{self, Kind};
use std::sync::mpsc::sync_channel;
use test_sink::{pkt, tracks, Call, FakeFactory, Log, MS};

/// Generations far above the live encoder tests' (the failure slot is global).
const G: u64 = 3_000_000;
const NAME: &str = "G__2026-11-10__13-00-00__rec.mp4";

fn video(pts_ms: u64, keyframe: bool, generation: u64) -> Data {
    Data::Video { pts_us: pts_ms * MS, keyframe, data: pkt(pts_ms as u8), generation }
}

/// J1: the part is listed (with its first frame's capture time) before the file exists, its
/// bookmarks are kept in the list as they come, and a stopped recording is no longer listed.
#[test]
fn a_long_recording_is_listed_while_it_is_written() {
    let sb = sandbox("journal-list");
    let path = sb.dir.join(NAME);
    let part = naming::part_of(&path);
    let log = Log::default();
    let mut rec = machine::Recorder::new(FakeFactory { log: log.clone(), ..Default::default() }, tracks());
    let t0 = Instant::now();
    rec.start(path.clone(), Some((0, t0)), t0);
    assert!(in_progress::get(&part).is_none(), "armed, no file yet: not listed");
    rec.video(100 * MS, true, pkt(1), false, t0);
    let entry = in_progress::get(&part).expect("listed once the part is opened");
    assert_eq!((entry.kind, entry.base_us), (Kind::Long, Some(100 * MS)));
    rec.mark(600 * MS);
    assert_eq!(in_progress::get(&part).map(|e| e.marks), Some(vec![600 * MS]), "a bookmark is kept in the list");
    rec.video(1_100 * MS, false, pkt(2), false, t0);
    rec.stop();
    assert!(path.is_file() && in_progress::get(&part).is_none(), "finished: no longer listed");
    assert!(log.calls().contains(&Call::Open(part)));
}

/// J2: recovery takes exactly what the list names, also outside the clip folder (the user has
/// changed it since), and leaves an unlisted, unrelated `.part` in another folder alone.
#[test]
fn a_listed_part_outside_the_clip_folder_is_recovered() {
    let sb = sandbox("journal-elsewhere");
    let (old_folder, clip_folder) = (sb.dir.join("old clips"), sb.dir.join("clips"));
    std::fs::create_dir_all(&old_folder).unwrap();
    std::fs::create_dir_all(&clip_folder).unwrap();
    let part = old_folder.join(format!("{NAME}.part"));
    std::fs::write(&part, b"fragments").unwrap();
    in_progress::add(in_progress::Entry::long(&part)).unwrap();
    let stranger = sb.dir.join("other").join(format!("{NAME}.part"));
    std::fs::create_dir_all(stranger.parent().unwrap()).unwrap();
    std::fs::write(&stranger, b"not listed").unwrap();
    let found = recover::run(&in_progress::list(), &clip_folder, None);
    let clip = old_folder.join(NAME);
    assert!(clip.is_file() && !part.exists(), "the listed part in the old clip folder is recovered: {:?}", found.logs);
    assert_eq!(found.long.iter().map(|r| r.clip.clone()).collect::<Vec<_>>(), vec![clip]);
    assert!(stranger.exists(), "an unlisted part elsewhere is never touched");
    assert!(in_progress::list().is_empty());
}

/// J3 (and restore test 9): the scan for older builds' `.part` files runs once, recovers what
/// it can, and never recycles: a replay part with no packet in it is kept, a long one renamed.
#[test]
fn the_legacy_scan_runs_once_and_never_recycles() {
    let sb = sandbox("journal-legacy");
    let long = sb.dir.join(format!("{NAME}.part"));
    std::fs::write(&long, b"fragments").unwrap();
    let empty = sb.dir.join("G__2026-11-10__10-01-00-000__bb.mp4.part");
    std::fs::write(&empty, [0, 0, 0, 16, b'f', b't', b'y', b'p', b'i', b's', b'o', b'm', 0, 0, 2, 0]).unwrap();
    // Older than the scan's freshness window (a save this run might still be writing).
    let old = std::time::SystemTime::now() - Duration::from_secs(120);
    std::fs::File::options().write(true).open(&empty).unwrap().set_modified(old).unwrap();
    let mut first = recover::Report::default();
    recover::legacy_scan(&sb.dir, None, &mut first);
    assert!(sb.dir.join(NAME).is_file(), "an older build's long recording is recovered: {:?}", first.logs);
    assert!(empty.exists(), "the one-time scan never recycles");
    assert!(first.logs.iter().any(|l| l.contains("bb.mp4.part")), "and says what it kept: {:?}", first.logs);
    assert!(in_progress::legacy_scanned(), "its marker is under the app-data root");
    let late = sb.dir.join("G__2026-11-10__14-00-00__rec.mp4.part");
    std::fs::write(&late, b"fragments").unwrap();
    let mut second = recover::Report::default();
    recover::legacy_scan(&sb.dir, None, &mut second);
    assert!(late.exists() && second.logs.is_empty() && second.long.is_empty(), "the second start scans nothing: {:?}", second.logs);
    assert!(crate::encoder::recycle::fake::binned_under(&sb.dir).is_empty());
}

/// U1 (long): a part that never got a video packet holds nothing of the recording: one
/// diagnostic line, then the Recycle Bin, never a permanent delete; one with data stays.
#[test]
fn an_empty_part_goes_to_the_recycle_bin_with_a_diagnostic_line() {
    let sb = sandbox("journal-empty");
    let part = sb.dir.join(format!("{NAME}.part"));
    std::fs::write(&part, [0, 0, 0, 16, b'f', b't', b'y', b'p', b'i', b's', b'o', b'm', 0, 0, 2, 0]).unwrap();
    let lines = dispose(&part);
    assert!(lines.iter().any(|l| l.contains("unrecoverable") && l.contains("boxes [ftyp 16]")), "a diagnostic line: {lines:?}");
    assert!(!part.exists() && crate::encoder::recycle::fake::binned_under(&sb.dir) == vec![part.clone()], "to the Recycle Bin");
    let kept = sb.dir.join("G__2026-11-10__13-30-00__rec.mp4.part");
    std::fs::write(&kept, b"some recording bytes").unwrap();
    dispose(&kept);
    assert!(kept.exists(), "a part with data in it is never recycled");
}

fn dispose(part: &Path) -> Vec<String> {
    machine::parts::dispose_empty_part(part).into_iter().filter_map(|e| if let Event::Log(l) = e { Some(l) } else { None }).collect()
}

fn writer(dir: &Path) -> Writer<FakeFactory> {
    let recorder = machine::Recorder::new(FakeFactory::default(), tracks());
    Writer { out_dir: dir.to_string_lossy().into_owned(), json: false, recorder, generation: 0, skew: Duration::ZERO, parts_so_far: Vec::new(), missed: (0.0, 0.0), lock: LockClock::default() }
}

/// P2 (engine): Resume recording continues a recovered recording as its next part, beside it.
#[test]
fn resume_recording_starts_the_next_part_of_the_recovered_one() {
    let sb = sandbox("journal-resume");
    let game = sb.dir.join("Rocket League");
    std::fs::create_dir_all(&game).unwrap();
    let recovered = game.join(NAME);
    std::fs::write(&recovered, b"recovered").unwrap();
    let mut w = writer(&sb.dir);
    let (_tx, rx) = sync_channel::<Data>(4);
    w.start("G", Some((0, Instant::now())), Some(&recovered));
    w.generation = G + 1;
    w.data(video(0, true, G + 1));
    w.data(video(16, false, G + 1));
    let next = game.join("G__2026-11-10__13-00-00__rec-2.mp4");
    assert!(naming::part_of(&next).is_file(), "the next part is being written beside the recovered one");
    w.stop(&rx);
    assert!(next.is_file() && recovered.is_file(), "both kept");
}

/// (e): recovery alone never starts a recording: nothing is armed, no new part is written.
#[test]
fn recovery_never_starts_a_recording() {
    let sb = sandbox("journal-no-start");
    let part = sb.dir.join(format!("{NAME}.part"));
    std::fs::write(&part, b"fragments").unwrap();
    in_progress::add(in_progress::Entry::long(&part)).unwrap();
    let w = writer(&sb.dir);
    let found = recover::run(&in_progress::list(), &sb.dir, None);
    assert_eq!(found.long.len(), 1, "{:?}", found.logs);
    assert!(!w.recorder.is_active(), "no recording armed");
    let parts: Vec<_> = std::fs::read_dir(&sb.dir).unwrap().flatten().filter(|e| e.file_name().to_string_lossy().ends_with(".part")).collect();
    assert!(parts.is_empty(), "no new part: {parts:?}");
    assert!(in_progress::list().is_empty(), "nothing is being written");
}
