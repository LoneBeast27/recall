mod start_at_tests {
    use super::super::*;

    #[test]
    fn a_trimmed_clip_opens_on_its_first_in_point() {
        assert_eq!(start_at(&[TimeRange::new(30.2, 60.0), TimeRange::new(70.0, 80.0)]), Some(30.2));
        assert_eq!(start_at(&[TimeRange::new(0.0, 60.0)]), None, "untrimmed: 0:00");
        assert_eq!(start_at(&[]), None);
    }

    #[test]
    fn the_playhead_shows_the_start_while_the_preview_loads_and_a_click_replaces_it() {
        let mut ed = EditorState::new(Err("no preview".into()));
        ed.plan.ranges = vec![TimeRange::new(30.2, 60.0)];
        ed.info = Some(ClipInfo { path: "a.mp4".into(), duration: 90.0, width: 4, height: 2, video_fps: 60.0, source_bytes: 1, source_modified_ms: 1, audio: Vec::new(), keyframes: Vec::new(), ..Default::default() });
        ed.pending_seek = start_at(&ed.plan.ranges);
        assert_eq!(ed.position(), 30.2);
        ed.seek(12.0);
        assert_eq!(ed.position(), 12.0, "a seek before the preview is ready waits in its place");
    }
}

mod same_path_tests {
    #[test]
    fn verbatim_and_plain_paths_match() {
        assert!(super::super::same_path(r"\\?\E:\Clips\a.mp4", r"e:\clips\a.mp4"));
        assert!(!super::super::same_path(r"\\?\E:\Clips\a.mp4", r"E:\Clips\b.mp4"));
    }
}

mod fps_cap_tests {
    use super::super::*;

    /// F-editor-export-4: a typed or saved frame rate above the largest preset is capped.
    #[test]
    fn a_typed_or_saved_frame_rate_is_capped_at_the_largest_preset() {
        assert_eq!(FPS_PRESETS.iter().max(), Some(&MAX_EXPORT_FPS));
        let mut ed = EditorState::new(Err("no preview".into()));
        ed.set_fps(9_999_999);
        assert_eq!(ed.fps, MAX_EXPORT_FPS, "a typo does not export millions of frames");
        ed.set_fps(144);
        assert_eq!(ed.fps, 144, "a sane custom rate stays");
        let info = ClipInfo { path: "a.mp4".into(), duration: 9.0, width: 4, height: 2, video_fps: 60.0, source_bytes: 1, source_modified_ms: 1, audio: Vec::new(), keyframes: Vec::new(), ..Default::default() };
        let plan = EditPlan { mix_down: None, ranges: vec![TimeRange::new(0.0, 9.0)], audio: Vec::new() };
        let project = crate::backend::Project { version: 1, source_path: info.path.clone(), source_bytes: 1, source_modified_ms: 1, plan, export_fps: 9_999_999, export: Default::default() };
        ed.adopt(info, Some(project), None);
        assert_eq!(ed.fps, 60, "edits saved before the cap open at the recorded rate");
    }

    fn clip(fps: f64) -> ClipInfo {
        ClipInfo { path: "a.mp4".into(), duration: 9.0, width: 4, height: 2, video_fps: fps, source_bytes: 1, source_modified_ms: 1, audio: Vec::new(), keyframes: Vec::new(), ..Default::default() }
    }

    /// Exports never go above the recorded rate; slow motion comes from Speed.
    #[test]
    fn export_rate_is_capped_at_the_recorded_rate() {
        let mut ed = EditorState::new(Err("no preview".into()));
        let info = clip(165.0);
        let plan = EditPlan { mix_down: None, ranges: vec![TimeRange::new(0.0, 9.0)], audio: Vec::new() };
        let project = crate::backend::Project { version: 1, source_path: info.path.clone(), source_bytes: 1, source_modified_ms: 1, plan, export_fps: 240, export: Default::default() };
        ed.adopt(info.clone(), Some(project), None);
        assert_eq!(ed.fps, 165, "a saved 240 on a 165 fps clip opens at 165");
        ed.info = Some(info);
        assert_eq!(ed.fps_choices(), vec![30, 60, 120, 144, 165], "only rates at or below the recording");
        ed.set_fps(480);
        assert_eq!(ed.fps, 165, "a typed rate above the recording is capped");
        ed.set_fps(100);
        assert_eq!(ed.fps, 100, "a lower custom rate stays");
    }

    #[test]
    fn a_new_clip_starts_at_60_or_its_recorded_rate_below_that() {
        let mut ed = EditorState::new(Err("no preview".into()));
        ed.adopt(clip(144.0), None, None);
        assert_eq!(ed.fps, 60, "the after-recording default: 60 (Rohan 2026-11-11)");
        ed.adopt(clip(59.94), None, None);
        assert_eq!(ed.fps, 60, "59.94 rounds to 60");
        ed.adopt(clip(30.0), None, None);
        assert_eq!(ed.fps, 30, "a 30 fps clip does not export at 60");
        assert_eq!(recorded_fps(&clip(0.0)), MAX_EXPORT_FPS, "an unreadable rate keeps the global cap");
        ed.info = Some(clip(30.0));
        assert_eq!(ed.fps_choices(), vec![30]);
    }
}

mod is_open_tests {
    use super::super::*;

    /// A Library tile names the clip through the recordings folder as configured; the editor holds
    /// the canonical path. Through a junction (a redirected Videos folder) they differ in text, and
    /// reopening the clip just left restarted it from 0:00 instead of where it stopped.
    #[test]
    fn a_clip_named_through_a_junction_is_the_clip_already_open() {
        let root = crate::test_temp::TempDir::sandbox("is-open-junction");
        let real = root.join("real");
        std::fs::create_dir_all(&real).unwrap();
        std::fs::write(real.join("a.mp4"), b"clip").unwrap();
        std::fs::hard_link(real.join("a.mp4"), real.join("b.mp4")).unwrap();
        let link = root.join("link");
        let made = std::process::Command::new("cmd").args(["/C", "mklink", "/J"]).arg(&link).arg(&real).output().unwrap();
        assert!(made.status.success(), "mklink /J: {}", String::from_utf8_lossy(&made.stderr));
        let mut ed = EditorState::new(Err("no preview".into()));
        let canonical = std::fs::canonicalize(real.join("a.mp4")).unwrap().to_string_lossy().into_owned();
        ed.info = Some(ClipInfo { path: canonical, duration: 9.0, width: 4, height: 2, video_fps: 60.0, source_bytes: 4, source_modified_ms: 1, audio: Vec::new(), keyframes: Vec::new(), ..Default::default() });
        assert!(ed.is_open(&link.join("a.mp4").to_string_lossy()), "the same file through the junction");
        assert!(ed.is_open(&real.join("a.mp4").to_string_lossy()));
        assert!(!ed.is_open(&link.join("b.mp4").to_string_lossy()), "a hard link is another clip");
        assert!(!ed.is_open(&link.join("missing.mp4").to_string_lossy()));
    }
}

mod upscale_tests {
    use super::super::*;
    use crate::edit_model::{ExportMode, ExportPriority};

    fn editor(height: u32, native: Option<u32>) -> EditorState {
        let mut ed = EditorState::new(Err("no preview".into()));
        ed.info = Some(ClipInfo { path: "a.mp4".into(), duration: 9.0, width: height * 16 / 9, height, video_fps: 60.0, native_height: native, ..Default::default() });
        ed
    }

    #[test]
    fn only_a_clip_recorded_below_its_game_offers_an_upscale() {
        assert!(editor(1440, None).upscale_choices().is_empty(), "native and older clips: none");
        assert_eq!(editor(1080, Some(1440)).upscale_choices(), vec![1440]);
        assert_eq!(editor(720, Some(2160)).upscale_choices(), vec![1080, 1440, 2160]);
    }

    #[test]
    fn an_upscale_and_an_up_front_resolution_replace_each_other() {
        let mut ed = editor(1080, Some(1440));
        ed.set_height(Some(720));
        ed.set_export_priority(ExportPriority::Motion);
        ed.set_upscale_height(Some(1440));
        assert_eq!((ed.export_settings.height, ed.upscale_height()), (None, Some(1440)));
        assert_eq!(ed.export_settings.priority, ExportPriority::Manual, "an upscale is a size picked by hand");
        ed.set_height(Some(720));
        assert_eq!((ed.export_settings.height, ed.upscale_height()), (Some(720), None));
    }

    #[test]
    fn an_upscale_counts_only_for_a_manual_re_encode() {
        let mut ed = editor(1080, Some(1440));
        ed.set_upscale_height(Some(1440));
        ed.set_export_mode(ExportMode::Lossless);
        assert_eq!(ed.upscale_height(), None, "lossless keeps the recording's size");
        ed.set_export_mode(ExportMode::Reencode);
        ed.set_max_bytes(Some(10_000_000));
        ed.set_export_priority(ExportPriority::Detail);
        assert_eq!(ed.upscale_height(), None, "a size fit picks its own size");
    }
}
