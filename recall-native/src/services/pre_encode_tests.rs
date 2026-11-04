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
    EditPlan { ranges: vec![TimeRange::new(start, end)], audio: vec![AudioSetting { stream_index: 1, gain_db: 0.0, muted: false, mix: true }] }
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
    let folder = std::env::temp_dir().join(format!("recall-pre-encode-src-{}", std::process::id()));
    std::fs::create_dir_all(&folder).unwrap();
    let clip = folder.join("clip.mp4");
    std::fs::write(&clip, b"12345").unwrap();
    let path = clip.to_string_lossy().into_owned();
    assert!(source_unchanged(&info(&path, 5)));
    assert!(!source_unchanged(&info(&path, 6)), "rewritten");
    std::fs::remove_file(&clip).unwrap();
    assert!(!source_unchanged(&info(&path, 5)), "moved away");
    let _ = std::fs::remove_dir_all(folder);
}
