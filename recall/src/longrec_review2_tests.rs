//! Restore policy review round 2, long-recording side, fail-first: bookmarks reach a recording
//! the list still names after it finished, a repair whose saved tail is gone for good ends as a
//! recovered recording (and one whose undo cannot run stops before the rename), and the rescan a
//! failed listing asks for also covers the former clip folders. Temp folders, a temp app-data
//! root, the fake Recycle Bin.

use super::restore_tests::{crashed_recording, sandbox};
use super::*;
use crate::encoder::in_progress::{self, Entry};
use std::io::Write as _;

const NAME: &str = "G__2026-11-11__12-00-00__rec.mp4";

/// P3: the part is gone because the recording did finish (the list was not updated): its
/// bookmarks from the list still reach the finished file.
#[test]
fn a_finished_recording_still_listed_gets_its_bookmarks() {
    let sb = sandbox("r2-marks-finished");
    let (part, clip) = (sb.dir.join(format!("{NAME}.part")), sb.dir.join(NAME));
    std::fs::write(&clip, b"finished").unwrap();
    in_progress::add(Entry { base_us: Some(100 * 1000), marks: vec![600 * 1000], ..Entry::long(&part) }).unwrap();
    let found = recover::run(&in_progress::list(), &sb.dir, None);
    let sidecar = crate::bookmarks::sidecar_path(&clip);
    assert_eq!(std::fs::read_to_string(&sidecar).ok().as_deref(), Some(r#"{"version":1,"marks":[0.5]}"#), "{:?}", found.logs);
}

/// A repair stopped after its cut, its undo record in place: (sandbox, part).
fn interrupted(tag: &str) -> Option<(super::restore_tests::Sandbox, std::path::PathBuf)> {
    let sb = sandbox(tag);
    let bytes = crashed_recording(&sb.dir)?;
    let part = sb.dir.join(format!("{NAME}.part"));
    std::fs::write(&part, &bytes[..bytes.len() * 77 / 100]).unwrap();
    let hooks = restore::Hooks { stop_after_patch: true, ..Default::default() };
    assert_eq!(restore::repair_with(&part, hooks), restore::Repair::Interrupted);
    Some((sb, part))
}

/// P3: the saved tail is gone for good and the file plays at its current length: it ends as a
/// recovered recording (its bytes kept, logged), never a `.part` forever.
#[test]
fn a_repair_whose_tail_is_gone_ends_as_a_recovered_recording() {
    let Some((sb, part)) = interrupted("r2-tail-gone") else { return eprintln!("libx264 missing; skipped") };
    crate::encoder::recycle::recycle(&restore::tail_path(&part)).unwrap();
    let found = recover::run(&in_progress::list(), &sb.dir, None);
    assert!(sb.dir.join(NAME).is_file() && !part.exists(), "recovered: {:?}", found.logs);
    assert!(found.logs.iter().any(|l| l.contains("tail")), "and says so: {:?}", found.logs);
    assert!(in_progress::list().is_empty());
}

/// An undo that cannot run (its saved tail changed) stops the recovery before the rename: the
/// part stays a listed `.part`, its tail beside it.
#[test]
fn a_stuck_repair_is_never_renamed() {
    let Some((sb, part)) = interrupted("r2-stuck") else { return eprintln!("libx264 missing; skipped") };
    std::fs::OpenOptions::new().append(true).open(restore::tail_path(&part)).unwrap().write_all(b"x").unwrap();
    let found = recover::run(&in_progress::list(), &sb.dir, None);
    assert!(part.is_file() && !sb.dir.join(NAME).exists(), "not renamed: {:?}", found.logs);
    assert!(in_progress::get(&part).is_some() && restore::tail_path(&part).is_file(), "still listed, tail kept");
}

/// P3: the rescan a failed listing asks for also covers the former clip folders (the app's
/// `folders.json`), where a resumed part beside an old recording sits.
#[test]
fn the_rescan_covers_the_former_clip_folders() {
    let sb = sandbox("r2-former");
    let (current, former) = (sb.dir.join("clips"), sb.dir.join("old clips"));
    std::fs::create_dir_all(&current).unwrap();
    std::fs::create_dir_all(&former).unwrap();
    let folders = crate::user_dirs::appdata().unwrap().join("Recall").join("folders.json");
    std::fs::create_dir_all(folders.parent().unwrap()).unwrap();
    let json = serde_json::json!({"recordings": current, "former_recordings": [former]});
    std::fs::write(&folders, json.to_string()).unwrap();
    let part = former.join("G__2026-11-11__12-00-00__rec-2.mp4.part");
    std::fs::write(&part, b"fragments").unwrap();
    let mut found = recover::Report::default();
    recover::legacy_scan(&current, None, &mut found);
    assert!(former.join("G__2026-11-11__12-00-00__rec-2.mp4").is_file(), "{:?}", found.logs);
}
