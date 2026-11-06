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
