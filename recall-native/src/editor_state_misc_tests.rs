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
