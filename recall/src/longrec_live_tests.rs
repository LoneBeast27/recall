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

/// The list's own temp file left by an engine that died mid-write is removed at start (only
/// that exact name, only for a pid that is not running); everything else stays.
#[test]
fn a_dead_engines_list_temp_file_is_removed_at_start() {
    let _sb = sandbox("live-temps");
    let dir = crate::encoder::in_progress::path().unwrap().parent().unwrap().to_path_buf();
    std::fs::create_dir_all(&dir).unwrap();
    let own = std::process::id();
    let names = [
        ("in-progress.json.4294967290.tmp", false),
        ("in-progress.legacy-scanned.4294967290.tmp", false),
        (&*format!("in-progress.json.{own}.tmp"), true),
        ("in-progress.json.4294967291.tmp", true),
        ("in-progress.json.12x.tmp", true),
        ("other.json.4294967290.tmp", true),
    ];
    for (name, _) in &names {
        std::fs::write(dir.join(name), b"{}").unwrap();
    }
    let lines = crate::encoder::in_progress::sweep_temps(&|pid| pid == 4_294_967_291);
    for (name, stays) in &names {
        assert_eq!(dir.join(name).exists(), *stays, "{name}: {lines:?}");
    }
    assert_eq!(lines.len(), 2, "one line per file removed: {lines:?}");
}

/// A listed entry whose file already finished (the engine died after the rename, before the
/// list was rewritten) is dropped at start with a line saying so.
#[test]
fn a_stale_entry_is_dropped_with_a_log_line() {
    let sb = sandbox("live-stale");
    let long_part = sb.dir.join("G__2026-11-11__17-00-00__rec.mp4.part");
    std::fs::write(sb.dir.join("G__2026-11-11__17-00-00__rec.mp4"), b"finished").unwrap();
    crate::encoder::in_progress::add(crate::encoder::in_progress::Entry::long(&long_part)).unwrap();
    let found = recover::run(&crate::encoder::in_progress::list(), &sb.dir, None);
    assert!(found.logs.iter().any(|l| l.contains("dropped") && l.contains("G__2026-11-11__17-00-00__rec.mp4.part")), "{:?}", found.logs);
    let replay_part = sb.dir.join("A__2026-11-11__17-00-00-000__aa.mp4.part");
    let clip = sb.dir.join("A__2026-11-11__17-00-00-000__aa.mp4");
    std::fs::write(&clip, b"saved").unwrap();
    let entry = crate::encoder::in_progress::Entry::replay(&replay_part, &clip, None);
    crate::encoder::in_progress::add(entry.clone()).unwrap();
    let (mut clips, mut logs) = (Vec::new(), Vec::new());
    crate::encoder::replay_rebuild::recover_entry(&entry, &mut clips, &mut logs);
    assert!(logs.iter().any(|l| l.contains("dropped") && l.contains("aa.mp4.part")), "{logs:?}");
    assert!(crate::encoder::in_progress::list().is_empty());
}
