use super::*;
use crate::edit_model::{ClipInfo, ExportMode, TimeRange};

fn editor() -> EditorState {
    let mut ed = EditorState::new(Err("no preview in tests".into()));
    ed.info = Some(ClipInfo {
        path: "E:/Recall/Raw/never-there.mp4".into(),
        duration: 120.0,
        width: 2560,
        height: 1440,
        video_fps: 60.0,
        source_bytes: 1,
        source_modified_ms: 1,
        audio: Vec::new(),
        keyframes: Vec::new(),
    });
    ed.plan.ranges = vec![TimeRange::new(0.0, 120.0)];
    ed
}

/// Before the settle time passes nothing starts, so these never launch the engine.
#[test]
fn only_a_change_arms_a_pre_encode_and_lossless_or_a_new_clip_disarms_it() {
    let _slot = crate::services::pre_encode::test_slot_lock();
    let mut ed = editor();
    ed.follow_pre_encode();
    assert!(ed.pre.current.is_some(), "the opened plan has a key");
    assert!(ed.pre.pending_since.is_none(), "an untouched clip is not prepared");

    ed.plan.ranges = vec![TimeRange::new(98.0, 120.0)];
    ed.follow_pre_encode();
    assert!(ed.pre.pending_since.is_some(), "a trimmed plan waits to settle, then prepares");
    let trimmed = ed.pre.current;

    ed.export_settings.max_bytes = Some(10_000_000);
    ed.follow_pre_encode();
    assert_ne!(ed.pre.current, trimmed, "a size limit is another output");
    assert!(ed.pre.pending_since.is_some());

    ed.plan.ranges = vec![TimeRange::new(0.0, 120.0)];
    ed.export_settings.max_bytes = None;
    ed.follow_pre_encode();
    assert!(ed.pre.pending_since.is_some(), "back to the opened plan after a change still prepares");

    ed.export_settings.mode = ExportMode::Lossless;
    ed.follow_pre_encode();
    assert_eq!((ed.pre.current, ed.pre.pending_since), (None, None), "lossless needs no pre-encode");
    assert_eq!(ed.pre_encode_line(), None);

    ed.drop_pre_encode();
    assert!(!ed.pre.touched && ed.pre.baseline.is_none(), "a new clip starts untouched");
}

#[test]
fn idle_ticks_reuse_the_key_and_a_change_recomputes_it() {
    let _slot = crate::services::pre_encode::test_slot_lock();
    let mut ed = editor();
    for _ in 0..20 {
        ed.follow_pre_encode();
    }
    assert_eq!(ed.pre.key_runs, 1, "no file stats or JSON on idle ticks");
    let opened = ed.pre.current;
    ed.plan.ranges = vec![TimeRange::new(98.0, 120.0)];
    ed.follow_pre_encode();
    ed.follow_pre_encode();
    assert_eq!(ed.pre.key_runs, 2, "one recompute per change");
    assert_ne!(ed.pre.current, opened);
    ed.export_settings.height = Some(720);
    ed.follow_pre_encode();
    assert_eq!(ed.pre.key_runs, 3, "a settings change recomputes too");
    if let Some((_, at)) = ed.pre.keyed.as_mut() {
        *at -= REKEY_AFTER;
    }
    ed.follow_pre_encode();
    assert_eq!(ed.pre.key_runs, 4, "an aged key is rechecked (engine build may have changed)");
}

#[test]
fn an_export_in_progress_freezes_the_plan_it_was_given() {
    let _slot = crate::services::pre_encode::test_slot_lock();
    let mut ed = editor();
    ed.follow_pre_encode();
    let opened = ed.pre.current;
    ed.exporting = true;
    ed.plan.ranges = vec![TimeRange::new(10.0, 20.0)];
    ed.tick_pre_encode();
    assert_eq!(ed.pre.current, opened, "no re-key (and no cancel) while Export waits on the pre-encode");
}

#[test]
fn a_pre_encode_needs_a_settled_edit_and_a_free_gpu() {
    assert!(may_start(true, false, false, true), "settled, GPU free, nothing running or prepared");
    assert!(!may_start(false, false, false, true), "the edit has not settled");
    assert!(!may_start(true, true, false, true), "recording, or no recorder to tell whether a game runs");
    assert!(!may_start(true, false, true, true), "one pre-encode at a time");
    assert!(!may_start(true, false, false, false), "already prepared or failed for this plan");
}

#[test]
fn without_the_recorder_the_gpu_is_held() {
    let _slot = crate::services::pre_encode::test_slot_lock();
    // Test processes have no recorder connection and no RECALL_NO_CONTROLLER: light copies and
    // pre-encodes both wait.
    crate::services::storage::set_recorder_connected(false);
    if std::env::var_os("RECALL_NO_CONTROLLER").is_none() {
        assert!(!crate::services::storage::gpu_work_allowed());
        assert!(gpu_held());
    }
    crate::services::storage::set_recorder_connected(true);
    if !crate::services::storage::recorder_busy() {
        assert!(!gpu_held(), "connected and idle: the pre-encode may run");
    }
    crate::services::storage::set_recorder_connected(false);
}


/// An export pressed before the edit settled (or while recording held the pre-encode) must
/// not be followed by a pre-encode of the plan it just exported.
#[test]
fn a_finished_export_disarms_the_pending_pre_encode() {
    let _slot = crate::services::pre_encode::test_slot_lock();
    let mut ed = editor();
    ed.follow_pre_encode();
    ed.plan.ranges = vec![TimeRange::new(98.0, 120.0)];
    ed.follow_pre_encode();
    assert!(ed.pre.pending_since.is_some(), "the trimmed plan waits to settle");
    ed.exporting = true;
    ed.export_of = ed.info.as_ref().map(|info| info.path.clone());
    let summary = crate::backend::ExportSummary { path: "E:/Recall/Edited/x.mp4".into(), fps: Some(60), frames: 1, seconds: 1.0, bytes: 1, lossless: false, width: 2, height: 2 };
    ed.on_exported(Ok(summary));
    assert!(ed.pre.pending_since.is_none(), "the plan just exported is not prepared again");
}

/// Lossless hides the size limit and resolution: their stored values must not reach the
/// engine, which refuses them for a stream copy.
#[test]
fn lossless_export_drops_the_hidden_reencode_settings() {
    let mut ed = editor();
    ed.export_settings.max_bytes = Some(10_000_000);
    ed.export_settings.height = Some(720);
    ed.export_settings.mode = ExportMode::Lossless;
    ed.export_settings.priority = crate::edit_model::ExportPriority::Detail;
    let info = ed.info.clone().unwrap();
    let crate::worker::Job::Export(_, plan, _, settings) = ed.export_job(info) else { panic!("an export job") };
    assert_eq!(settings.validate(&plan), Ok(()), "{settings:?}");
    assert_eq!(settings.priority, crate::edit_model::ExportPriority::Manual);
    assert_eq!((ed.export_settings.max_bytes, ed.export_settings.height), (Some(10_000_000), Some(720)), "switching back restores them");
}

/// The Export button and the summary say what the export does: progress once the engine
/// reports it, no frame rate for a stream copy, and the output size after Resolution.
#[test]
fn the_export_button_and_size_describe_the_output() {
    use crate::editor_state::persist::export_label;
    assert_eq!(export_label(Some(60), None), "Exporting at 60 fps…");
    assert_eq!(export_label(Some(60), Some(0.427)), "Exporting at 60 fps… 42%");
    assert_eq!(export_label(None, Some(0.5)), "Exporting… 50%");
    assert_eq!(export_label(None, None), "Exporting…");
    let mut ed = editor();
    assert_eq!(ed.export_size(), Some((2560, 1440)));
    ed.export_settings.height = Some(720);
    assert_eq!(ed.export_size(), Some((1280, 720)), "the Resolution choice");
    ed.export_settings.mode = ExportMode::Lossless;
    assert_eq!(ed.export_size(), Some((2560, 1440)), "a stream copy keeps the source size");
}
