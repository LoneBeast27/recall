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
        ed.info = Some(ClipInfo { path: "a.mp4".into(), duration: 90.0, width: 4, height: 2, video_fps: 60.0, source_bytes: 1, source_modified_ms: 1, audio: Vec::new(), keyframes: Vec::new() });
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
        let info = ClipInfo { path: "a.mp4".into(), duration: 9.0, width: 4, height: 2, video_fps: 60.0, source_bytes: 1, source_modified_ms: 1, audio: Vec::new(), keyframes: Vec::new() };
        let plan = EditPlan { mix_down: None, ranges: vec![TimeRange::new(0.0, 9.0)], audio: Vec::new() };
        let project = crate::backend::Project { version: 1, source_path: info.path.clone(), source_bytes: 1, source_modified_ms: 1, plan, export_fps: 9_999_999, export: Default::default() };
        ed.adopt(info, Some(project), None);
        assert_eq!(ed.fps, MAX_EXPORT_FPS, "edits saved before the cap open capped");
    }
}
