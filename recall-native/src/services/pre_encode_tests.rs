use super::*;
use crate::edit_model::{AudioSetting, TimeRange};

fn info(path: &str, bytes: u64) -> ClipInfo {
    ClipInfo {
        path: path.into(),
        duration: 120.0,
        width: 2560,
        height: 1440,
        video_fps: 60.0,
        source_bytes: bytes,
        source_modified_ms: 1,
        audio: Vec::new(),
        keyframes: Vec::new(),
    }
}

fn plan(start: f64, end: f64) -> EditPlan {
    EditPlan { mix_down: None, ranges: vec![TimeRange::new(start, end)], audio: vec![AudioSetting { stream_index: 1, gain_db: 0.0, muted: false, mix: true }] }
}

fn summary(path: &Path) -> ExportSummary {
    ExportSummary { path: path.to_string_lossy().into_owned(), fps: Some(60), frames: 60, seconds: 1.0, bytes: 3, lossless: false, width: 2, height: 2 }
}

#[test]
fn the_key_follows_everything_that_shapes_the_output() {
    let clip = info("E:/Recall/Raw/a.mp4", 100);
    let settings = ExportSettings::default();
    let base = key(&clip, &plan(98.0, 120.0), 60, &settings).expect("re-encode has a key");
    assert_eq!(key(&clip, &plan(98.0, 120.0), 60, &settings), Some(base), "same inputs, same key");
    assert_ne!(key(&clip, &plan(97.0, 120.0), 60, &settings), Some(base), "a moved cut");
    assert_ne!(key(&clip, &plan(98.0, 120.0), 30, &settings), Some(base), "another fps");
    let capped = ExportSettings { max_bytes: Some(10_000_000), ..settings.clone() };
    assert_ne!(key(&clip, &plan(98.0, 120.0), 60, &capped), Some(base), "a size limit");
    let smaller = ExportSettings { height: Some(720), ..settings.clone() };
    assert_ne!(key(&clip, &plan(98.0, 120.0), 60, &smaller), Some(base), "a resolution");
    assert_ne!(key(&info("E:/Recall/Raw/a.mp4", 101), &plan(98.0, 120.0), 60, &settings), Some(base), "a rewritten clip");
    assert_ne!(key(&info("F:/Recall/Raw/a.mp4", 100), &plan(98.0, 120.0), 60, &settings), Some(base), "a moved clip");
    let lossless = ExportSettings { mode: ExportMode::Lossless, ..settings };
    assert_eq!(key(&clip, &plan(98.0, 120.0), 60, &lossless), None, "lossless is already a copy");
}

#[test]
fn only_engine_progress_lines_count() {
    assert_eq!(progress_share(r#"{"type":"export_progress","frames":330,"total":1320}"#), Some(0.25));
    assert_eq!(progress_share(r#"{"type":"export_progress","frames":9,"total":0}"#), None);
    assert_eq!(progress_share(r#"{"type":"exported","frames":1320}"#), None);
    assert_eq!(progress_share("[aac @ 0x1] Qavg: 634.499"), None);
}

#[test]
fn one_prepared_file_matches_only_its_own_key_and_is_handed_over_once() {
    let _slot = test_slot_lock();
    let folder = std::env::temp_dir().join(format!("recall-pre-encode-test-{}", std::process::id()));
    std::fs::create_dir_all(&folder).unwrap();
    let file = folder.join("a.mp4");
    std::fs::write(&file, b"mp4").unwrap();
    slot().ready = Some(Prepared { key: 7, path: file.clone(), summary: summary(&file) });

    assert_eq!(status(7), Status::Ready);
    assert_eq!(status(8), Status::None);
    cancel_other(Some(7));
    assert!(file.exists(), "the matching file stays");
    assert!(take_ready(8, || false).is_none(), "another plan never gets it");
    let taken = take_ready(7, || false).expect("the matching plan gets it");
    assert_eq!(taken.path, file);
    assert_eq!(status(7), Status::None, "handed over once");

    slot().ready = Some(taken);
    cancel_other(Some(9));
    assert!(!file.exists(), "a new plan drops the old file: one at a time");
    assert_eq!(status(7), Status::None);

    slot().failed = Some((9, "nvenc".into()));
    assert_eq!(status(9), Status::Failed("nvenc".into()));
    cancel_other(None);
    assert_eq!(status(9), Status::None);
    let _ = std::fs::remove_dir_all(folder);
}

#[test]
fn the_start_sweep_removes_only_cut_off_deliveries() {
    let folder = std::env::temp_dir().join(format!("recall-pre-encode-sweep-{}", std::process::id()));
    std::fs::create_dir_all(&folder).unwrap();
    let keep = [
        "clip-edited-60fps-1.mp4",
        "clip-edited-60fps-1.mp4.recall-pre-encode",
        ".clip-edited-60fps-1.mp4.recall-export-77-0.mp4",
        ".recall-pre-encode",
        "notes.txt",
    ];
    for name in keep.iter().chain([".clip-edited-60fps-2.mp4.recall-pre-encode"].iter()) {
        std::fs::write(folder.join(name), b"x").unwrap();
    }
    std::fs::create_dir_all(folder.join(".folder.recall-pre-encode")).unwrap();
    assert_eq!(remove_deliveries(&folder), 1);
    assert!(!folder.join(".clip-edited-60fps-2.mp4.recall-pre-encode").exists());
    for name in keep {
        assert!(folder.join(name).exists(), "{name} must stay");
    }
    assert!(folder.join(".folder.recall-pre-encode").is_dir(), "folders stay");
    assert_eq!(remove_deliveries(&folder.join("missing")), 0);
    let _ = std::fs::remove_dir_all(folder);
}

#[test]
fn a_moved_or_rewritten_clip_is_a_mismatch() {
    let sandbox = crate::test_temp::TempDir::sandbox("recall-pre-encode-src");
    let clip = sandbox.join("clip.mp4");
    std::fs::write(&clip, b"12345").unwrap();
    let path = clip.to_string_lossy().into_owned();
    let written = |bytes| ClipInfo { source_modified_ms: backend::modified_ms(&clip.metadata().unwrap()), ..info(&path, bytes) };
    assert!(source_unchanged(&written(5)));
    assert!(!source_unchanged(&written(6)), "rewritten");
    std::fs::remove_file(&clip).unwrap();
    assert!(!source_unchanged(&info(&path, 5)), "moved away");
}

/// F-editor-export-3: a clip rewritten to the same size (only its write time moved) must not
/// get the old pre-encode; the normal export refuses it the same way.
#[test]
fn a_clip_rewritten_to_the_same_size_is_a_mismatch() {
    let sandbox = crate::test_temp::TempDir::sandbox("recall-pre-encode-mtime");
    let clip = sandbox.join("clip.mp4");
    std::fs::write(&clip, b"12345").unwrap();
    let path = clip.to_string_lossy().into_owned();
    let opened = backend::modified_ms(&clip.metadata().unwrap());
    let stale = ClipInfo { source_modified_ms: opened.saturating_sub(5_000), ..info(&path, 5) };
    assert!(!source_unchanged(&stale), "same size, other write time");
    assert!(source_unchanged(&ClipInfo { source_modified_ms: opened, ..info(&path, 5) }));
}

#[test]
fn clearing_the_pre_encode_folder_spares_files_recall_did_not_make() {
    let sandbox = crate::test_temp::TempDir::sandbox("recall-pre-encode-clear");
    let root = sandbox.join("cache");
    let folder = root.join("tmp").join("pre-encode");
    std::fs::create_dir_all(&folder).unwrap();
    let user = folder.join("My clip.mp4");
    let shaped = folder.join("0123456789abcdef.mp4");
    let own = folder.join("00000000000000aa.mp4");
    let own_temp = folder.join(".00000000000000aa.recall-export-1-2-0.mp4");
    // Recall lists its prepared file before the engine writes it; the user's files come later.
    storage::mark_owned(&root, "tmp/pre-encode/00000000000000aa.mp4");
    for file in [&user, &shaped, &own, &own_temp] {
        std::fs::write(file, b"bytes").unwrap();
    }
    clear_in(&root, None);
    assert!(user.exists() && shaped.exists(), "a file Recall did not make is left alone");
    assert!(!own.exists() && !own_temp.exists(), "Recall's prepared file and its engine temp go");
}

/// An engine that crashed, or was killed with the app, leaves its hidden temp beside the
/// export for good: the start sweep removes it when its process is gone, nothing else.
#[test]
fn the_start_sweep_removes_a_dead_engines_export_temp() {
    let sandbox = crate::test_temp::TempDir::sandbox("recall-export-temp-sweep");
    let folder = sandbox.join("Edited");
    std::fs::create_dir_all(&folder).unwrap();
    let dead = ".a-edited-60fps-1.recall-export-4000000000-5-0.mp4";
    let live = format!(".b-edited-60fps-1.recall-export-{}-5-0.mp4", std::process::id());
    let keep = [".a.mp4", "a-edited-60fps-1.mp4", ".a.recall-export-x-5-0.mp4", ".a.recall-export-1-5.mp4", live.as_str()];
    for name in keep.iter().chain([&dead]) {
        std::fs::write(folder.join(name), b"x").unwrap();
    }
    assert_eq!(remove_deliveries(&folder), 1);
    assert!(!folder.join(dead).exists(), "the dead engine's partial export goes");
    for name in keep {
        assert!(folder.join(name).exists(), "{name} must stay");
    }
}

/// Closing the window while Export waits on a running pre-encode of the same plan: that
/// pre-encode keeps running (the export then only copies it); nothing re-encodes from zero.
#[test]
fn closing_spares_the_pre_encode_a_queued_export_waits_on() {
    let _slot = test_slot_lock();
    CANCEL.store(false, Ordering::Release);
    slot().running = Some(41);
    let run = export_run(Some(41)); // the export is running and waits on it
    discard();
    assert!(!CANCEL.load(Ordering::Acquire), "the awaited pre-encode is not stopped");
    assert!(awaited(), "closing waits for that export");
    drop(run);
    cancel_other(None);
    assert!(CANCEL.load(Ordering::Acquire), "with no export waiting it is stopped");
    slot().running = None;
    CANCEL.store(false, Ordering::Release);
}

/// R2-2: Export was clicked but its job still waits in the io queue when the window closes.
/// The pre-encode it will copy must keep running; else the export encodes from zero.
#[test]
fn closing_before_a_queued_export_starts_spares_its_pre_encode() {
    let _slot = test_slot_lock();
    CANCEL.store(false, Ordering::Release);
    slot().running = Some(42);
    await_export(Some(42)); // the Export click; the job has not run yet
    discard();
    let cancelled = CANCEL.load(Ordering::Acquire);
    {
        let mut slot = slot();
        slot.running = None;
        slot.awaited = None;
    }
    CANCEL.store(false, Ordering::Release);
    assert!(!cancelled, "the pre-encode the queued export waits on was stopped");
}

/// R2-3: from the moment the export takes the prepared file until the copy ends, closing the
/// app waits (`awaited`); there is no gap in which the app can exit before the copy.
#[test]
fn closing_waits_from_the_hand_over_to_the_end_of_the_copy() {
    let _slot = test_slot_lock();
    let sandbox = crate::test_temp::TempDir::sandbox("recall-pre-encode-hand-over");
    let file = sandbox.join("a.mp4");
    std::fs::write(&file, b"mp4").unwrap();
    slot().ready = Some(Prepared { key: 5, path: file.clone(), summary: summary(&file) });
    let run = export_run(Some(5));
    let taken = take_ready(5, || false);
    let waited = awaited();
    drop(run);
    assert!(taken.is_some(), "the prepared file is handed over");
    assert!(waited, "between the hand-over and the copy the app could exit");
    assert!(!awaited(), "the export ended: nothing holds the close any more");
}

/// X8: what each delivery outcome does with the prepared file.
#[test]
fn a_full_or_locked_export_folder_keeps_the_prepared_file_for_the_next_try() {
    let _slot = test_slot_lock();
    let sandbox = crate::test_temp::TempDir::sandbox("recall-pre-encode-settle");
    let prepared = |name: &str, key: u64| {
        let path = sandbox.join(name);
        std::fs::write(&path, b"mp4").unwrap();
        Prepared { key, path: path.clone(), summary: summary(&path) }
    };
    slot().ready = None;
    let kept = prepared("kept.mp4", 11);
    let result = settle(kept.clone(), Err(Delivery::Kept("disk full".into())));
    assert_eq!(result.map(|r| r.unwrap_err()), Some("disk full".to_owned()));
    assert!(kept.path.exists(), "the prepared file stays");
    assert_eq!(status(11), Status::Ready, "and is offered to the next Export");

    slot().ready = None;
    let cancelled = prepared("cancelled.mp4", 12);
    let result = settle(cancelled.clone(), Err(Delivery::Cancelled));
    assert_eq!(result.map(|r| r.unwrap_err()), Some(backend::EXPORT_CANCELLED.to_owned()));
    assert!(cancelled.path.exists() && status(12) == Status::Ready);

    slot().ready = None;
    let failed = prepared("failed.mp4", 13);
    assert!(settle(failed.clone(), Err(Delivery::Failed("odd".into()))).is_none(), "Export encodes instead");
    assert!(!failed.path.exists() && status(13) == Status::None);

    let done = prepared("done.mp4", 14);
    let result = settle(done.clone(), Ok(summary(&sandbox.join("out.mp4"))));
    assert!(matches!(result, Some(Ok(_))));
    assert!(!done.path.exists(), "a delivered file leaves the cache");
    slot().ready = None;
}

/// Post-hoc item 8: a size-fit priority with no size cap in force changes nothing in the
/// output, so it must not change the key (or the export is prepared again for nothing).
#[test]
fn a_priority_without_a_size_cap_keeps_the_key() {
    use crate::edit_model::ExportPriority;
    let clip = info("E:/Recall/Raw/a.mp4", 100);
    let manual = ExportSettings::default();
    let motion = ExportSettings { priority: ExportPriority::Motion, ..manual.clone() };
    assert_eq!(key(&clip, &plan(98.0, 120.0), 60, &motion), key(&clip, &plan(98.0, 120.0), 60, &manual), "no cap: no priority");
    let capped = ExportSettings { max_bytes: Some(10_000_000), ..manual.clone() };
    let capped_motion = ExportSettings { priority: ExportPriority::Motion, ..capped.clone() };
    assert_ne!(key(&clip, &plan(98.0, 120.0), 60, &capped_motion), key(&clip, &plan(98.0, 120.0), 60, &capped), "under a cap it shapes the output");
}
