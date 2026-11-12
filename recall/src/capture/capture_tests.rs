//! Capture restart and rate-cap tests (no GPU, no display).

mod restart_tests {
    use std::sync::atomic::Ordering::Relaxed;

    /// The DDA thread's give-up (device lost, re-create failing) sets `dda_failed` and
    /// `restart`: the engine's periodic check restarts capture once, and the new start skips
    /// Desktop Duplication even when it is opted in, so it lands on WGC.
    #[test]
    fn a_failed_desktop_duplication_restarts_capture_on_wgc() {
        let shared = super::super::CaptureShared::new();
        shared.fp16.store(true, Relaxed);
        assert_eq!(shared.restart_reason(), None, "a healthy capture keeps running");
        shared.dda_failed.store(true, Relaxed);
        shared.restart.store(true, Relaxed);
        assert_eq!(shared.restart_reason(), Some("(desktop duplication stopped; WGC from now on)"));
        assert_eq!(shared.restart_reason(), None, "one restart per request");
        assert!(super::super::dda_logic::dda_wanted(Some("dda"), shared.dda_failed.load(Relaxed)).is_err(), "the restart lands on WGC");
    }

    #[test]
    fn a_followed_window_leaving_its_monitor_restarts_without_disabling_dda() {
        let shared = super::super::CaptureShared::new();
        shared.fp16.store(true, Relaxed);
        shared.restart.store(true, Relaxed);
        assert_eq!(shared.restart_reason(), Some("(capture source changed)"));
        assert!(super::super::dda_logic::dda_wanted(Some("dda"), shared.dda_failed.load(Relaxed)).is_ok());
    }
}

mod rate_cap_tests {
    #[test]
    fn interval_leaves_ten_percent_headroom_over_the_recording_rate() {
        let at60 = super::super::start::capture_interval(60);
        assert!((1.0 / at60.as_secs_f64() - 66.0).abs() < 0.01);
        assert!((1.0 / super::super::start::capture_interval(144).as_secs_f64() - 158.4).abs() < 0.01);
        assert_eq!(super::super::start::capture_interval(0), super::super::start::capture_interval(1));
    }
}

mod soft_cap_tests {
    use std::sync::atomic::Ordering::Relaxed;

    /// Windows before 11 24H2 has no `MinUpdateInterval`: the handler keeps the rate itself,
    /// so a 400 fps game does not get 400 tonemaps and NVENC submits a second.
    #[test]
    fn a_handler_keeps_the_rate_when_windows_cannot() {
        let shared = super::super::CaptureShared::new();
        assert!(super::super::Handler::from_shared(shared.clone()).soft_cap.is_none(), "capped by Windows: no second cap");
        shared.soft_cap_ns.store(15_151_515, Relaxed);
        let mut handler = super::super::Handler::from_shared(shared.clone());
        let pacer = handler.soft_cap.as_mut().expect("uncapped Windows: the handler paces");
        let start = std::time::Instant::now();
        let kept = (0..400u64)
            .filter(|i| pacer.offer(start + std::time::Duration::from_micros(i * 2_500)) == super::super::dda_logic::Pace::Deliver)
            .count();
        assert!((65..=67).contains(&kept), "one second at 400 fps keeps about 66 frames, kept {kept}");
        assert!(super::super::Handler::from_shared(shared).paced_elsewhere().soft_cap.is_none(), "Desktop Duplication paces its own acquires");
    }
}
