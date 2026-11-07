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
    assert!(validate_project(&saved, &info(r"\\?\F:\Recall\Raw\clip.mp4")).is_ok());
    assert!(validate_project(&saved, &info(r"\\?\E:\Moved\CLIP.mp4")).is_ok(), "the name compares like Windows does");
}

#[test]
fn edits_of_another_clip_or_revision_still_do_not_load() {
    let saved = project(r"\\?\E:\Recall\Raw\clip.mp4");
    assert!(validate_project(&saved, &info(r"\\?\F:\Recall\Raw\other.mp4")).is_err(), "another name");
    let mut changed = info(r"\\?\E:\Recall\Raw\clip.mp4");
    changed.source_bytes += 1;
    assert!(validate_project(&saved, &changed).is_err(), "another size");
    let mut touched = info(r"\\?\E:\Recall\Raw\clip.mp4");
    touched.source_modified_ms += 1;
    assert!(validate_project(&saved, &touched).is_err(), "another write time");
}
