//! Restore policy, fail-first (`.planning/RESTORE-POLICY.md` (d) tests 8, 9): the minimal safe
//! handling of a replay save's `.part` at engine start. Real libx264 MP4 bytes (CPU only,
//! skipped without libx264), temp folders only; nothing here is ever deleted by the code.

use std::time::Duration;

use super::tests::{cut_before_moov, finished_clip};
use super::super::test_scratch::scratch;
use super::*;

/// Restore test 8: "Sort clips into game folders" saves a replay clip in `<out>\<Game>\`, so its
/// `.part` is there; the start-up scan finds it like one at the top level (today it never looks).
#[test]
fn a_replay_part_in_a_game_folder_is_found_at_start() {
    let dir = scratch("recall-restore", "replay-game");
    let Some(whole) = finished_clip(&dir) else { return eprintln!("libx264 missing; skipped") };
    let game = dir.join("Rocket League");
    crate::game_folder_rule::make(&game).unwrap();
    // Finished (its moov is written), only the rename was lost.
    let part = game.join("Rocket-League__2026-11-10__10-00-00-000__aa.mp4.part");
    std::fs::write(&part, &whole).unwrap();
    let found = recover_replay_parts_older(&dir, Duration::ZERO);
    let clip = game.join("Rocket-League__2026-11-10__10-00-00-000__aa.mp4");
    assert_eq!(found, vec![Found::Adopted { part: part.clone(), clip: clip.clone() }], "the replay .part in the game folder is found at start");
    assert!(clip.is_file() && !part.exists());
}

/// Restore test 9: a replay save cut off mid-write (no index) is kept, and logged once, not at
/// every start (today the same line comes back at each start).
#[test]
fn a_cut_off_replay_part_is_logged_once_and_kept() {
    let dir = scratch("recall-restore", "replay-once");
    let Some(whole) = finished_clip(&dir) else { return eprintln!("libx264 missing; skipped") };
    let part = dir.join("G__2026-11-10__10-01-00-000__bb.mp4.part");
    let cut = cut_before_moov(&whole);
    std::fs::write(&part, &cut).unwrap();
    let name = part.file_name().unwrap().to_string_lossy().into_owned();
    let lines = |found: Vec<Found>| found.iter().map(Found::line).filter(|l| l.contains(&name)).collect::<Vec<_>>();
    let first = lines(recover_replay_parts_older(&dir, Duration::ZERO));
    assert_eq!(first.len(), 1, "the first start says it was cut off: {first:?}");
    let second = lines(recover_replay_parts_older(&dir, Duration::ZERO));
    assert!(second.is_empty(), "the second start logs the same cut-off replay save again: {second:?}");
    assert_eq!(std::fs::read(&part).unwrap(), cut, "kept byte for byte");
}
