//! Saved edits follow the clip, not its absolute path (`validate_project`).
use super::*;
use crate::edit_model::TimeRange;

fn info(path: &str) -> ClipInfo {
    ClipInfo { path: path.into(), duration: 10.0, width: 1, height: 1, video_fps: 60.0, source_bytes: 1234, source_modified_ms: 5678, audio: Vec::new(), keyframes: Vec::new() }
}

fn project(path: &str) -> Project {
    Project { version: 1, source_path: path.into(), source_bytes: 1234, source_modified_ms: 5678, plan: EditPlan { mix_down: None, ranges: vec![TimeRange::new(1.0, 9.0)], audio: Vec::new() }, export_fps: 60, export: ExportSettings::default() }
}

#[test]
fn edits_still_load_after_the_drive_letter_or_folder_changes() {
    // E4: a USB drive back as F: instead of E:, or Recall\Raw moved: the same clip.
    let saved = project(r"\\?\E:\Recall\Raw\clip.mp4");
    assert!(validate_project(&saved, &info(r"\\?\F:\Recall\Raw\clip.mp4"), ProjectSource::Sidecar).is_ok());
    assert!(validate_project(&saved, &info(r"\\?\E:\Moved\CLIP.mp4"), ProjectSource::Sidecar).is_ok(), "the name compares like Windows does");
}

#[test]
fn edits_of_another_clip_or_revision_still_do_not_load() {
    let saved = project(r"\\?\E:\Recall\Raw\clip.mp4");
    assert!(validate_project(&saved, &info(r"\\?\F:\Recall\Raw\other.mp4"), ProjectSource::Sidecar).is_err(), "another name");
    let mut changed = info(r"\\?\E:\Recall\Raw\clip.mp4");
    changed.source_bytes += 1;
    assert!(validate_project(&saved, &changed, ProjectSource::Sidecar).is_err(), "another size");
    let mut touched = info(r"\\?\E:\Recall\Raw\clip.mp4");
    touched.source_modified_ms += 1;
    assert!(validate_project(&saved, &touched, ProjectSource::Sidecar).is_err(), "another write time");
}

#[test]
fn a_moved_clip_on_a_gone_path_keeps_its_working_copy() {
    // The recorded path no longer exists (another drive letter): a move, not a copy.
    let saved = project(r"\\?\Q:\Gone\Recall\clip.mp4");
    assert!(validate_project(&saved, &info(r"\\?\F:\Recall\Raw\clip.mp4"), ProjectSource::WorkingCopy).is_ok());
}

#[test]
fn a_byte_identical_copy_starts_clean_even_once_the_other_is_gone() {
    // R2-1: an Explorer copy keeps name, size and write time, so both copies shared one SSD
    // working copy; the copy opened the other's cut and its next flush overwrote that sidecar.
    let dir = std::env::temp_dir().join(format!("recall-copy-edits-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    for side in ["a", "b"] {
        std::fs::create_dir_all(dir.join(side)).unwrap();
        std::fs::write(dir.join(side).join("r2-1-copy.mp4"), b"same bytes").unwrap();
    }
    let path = |side: &str| std::fs::canonicalize(dir.join(side).join("r2-1-copy.mp4")).unwrap().to_string_lossy().into_owned();
    let (a, b) = (info(&path("a")), info(&path("b")));
    let plan = EditPlan { mix_down: None, ranges: vec![TimeRange::new(2.0, 8.0)], audio: Vec::new() };
    save_working(&a, &plan, 60, &ExportSettings::default()).unwrap();
    assert!(load_project(&b).0.is_none(), "the copy must not open the other copy's cut");
    assert_eq!(load_project(&a).0.map(|p| p.plan.ranges), Some(plan.ranges.clone()), "the clip itself keeps its edits");
    // A sidecar beside the clip always counts: its place names the clip.
    let mut beside = project(&path("a"));
    beside.source_bytes = b.source_bytes;
    assert!(validate_project(&beside, &b, ProjectSource::Sidecar).is_ok());
    // The other copy gone: b is still another file (another file id) and starts clean.
    std::fs::remove_file(dir.join("a").join("r2-1-copy.mp4")).unwrap();
    assert!(load_project(&b).0.is_none());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn two_copies_edited_in_turn_each_keep_their_own_edits_and_a_move_keeps_its_id() {
    // R2-1 follow-up: both copies shared one working-copy file, so each save overwrote the other's.
    use super::working::tests::inject;
    let dir = std::env::temp_dir().join(format!("recall-copy-ids-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    for side in ["a", "b", "moved"] {
        std::fs::create_dir_all(dir.join(side)).unwrap();
    }
    for side in ["a", "b"] {
        std::fs::write(dir.join(side).join("r2-1-ids.mp4"), b"same bytes").unwrap();
    }
    let path = |side: &str| std::fs::canonicalize(dir.join(side)).unwrap().join("r2-1-ids.mp4").to_string_lossy().into_owned();
    let (a, b) = (info(&path("a")), info(&path("b")));
    inject(&a.path, Some((7, 100)));
    inject(&b.path, Some((7, 101)));
    let plan = |start: f64| EditPlan { mix_down: None, ranges: vec![TimeRange::new(start, 9.0)], audio: Vec::new() };
    let ranges = |clip: &ClipInfo| load_project(clip).0.map(|p| p.plan.ranges);
    save_working(&a, &plan(2.0), 60, &ExportSettings::default()).unwrap();
    save_working(&b, &plan(3.0), 60, &ExportSettings::default()).unwrap();
    save_working(&a, &plan(4.0), 60, &ExportSettings::default()).unwrap();
    assert_eq!(ranges(&a), Some(plan(4.0).ranges), "copy a keeps its own edits");
    assert_eq!(ranges(&b), Some(plan(3.0).ranges), "copy b keeps its own edits");
    // A move within the volume keeps the file id, and with it the edits.
    std::fs::rename(dir.join("a").join("r2-1-ids.mp4"), dir.join("moved").join("r2-1-ids.mp4")).unwrap();
    let moved = info(&path("moved"));
    inject(&moved.path, Some((7, 100)));
    assert_eq!(ranges(&moved), Some(plan(4.0).ranges), "the moved clip keeps its edits");
    // No id (network share, FAT): the name, size and write time key, as before.
    inject(&b.path, None);
    save_working(&b, &plan(5.0), 60, &ExportSettings::default()).unwrap();
    assert_eq!(ranges(&b), Some(plan(5.0).ranges));
    let _ = std::fs::remove_dir_all(&dir);
}
