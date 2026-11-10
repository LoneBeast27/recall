//! Restore policy, fail-first (`.planning/RESTORE-POLICY.md` (d) tests 8, 9): the minimal safe
//! handling of a replay save's `.part` at engine start. Real libx264 MP4 bytes (CPU only,
//! skipped without libx264), temp folders only; nothing here is ever deleted by the code.

use std::time::Duration;

use super::tests::finished_clip;
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

// Restore test 9 (a cut-off replay part logged once) is `longrec::journal_tests::the_legacy_scan_runs_once_and_never_recycles`:
// the scan that finds an older build's parts now runs once.
