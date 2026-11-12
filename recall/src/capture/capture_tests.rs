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
    use std::time::Duration;

    use super::super::soft_cap::SoftCap;

    const MS: i64 = 10_000; // 100 ns units

    /// Windows before 11 24H2 has no `MinUpdateInterval`: the handler keeps the rate itself,
    /// so a 400 fps game does not get 400 tonemaps and NVENC submits a second.
    #[test]
    fn a_handler_keeps_the_rate_only_when_windows_cannot() {
        let shared = super::super::CaptureShared::new();
        assert!(super::super::Handler::from_shared(shared.clone()).soft_cap.is_none(), "capped by Windows: no second cap");
        shared.soft_cap_ns.store(15_151_515, Relaxed);
        assert!(super::super::Handler::from_shared(shared.clone()).soft_cap.is_some(), "uncapped Windows: the handler paces");
        assert!(super::super::Handler::from_shared(shared).paced_elsewhere().soft_cap.is_none(), "Desktop Duplication paces its own acquires");
    }

    #[test]
    fn one_second_at_400_fps_keeps_66_frames() {
        let mut cap = SoftCap::new(super::super::start::capture_interval(60));
        let kept = (0..400i64).filter(|i| cap.keep(i * 25 * MS / 10)).count();
        assert!((65..=67).contains(&kept), "kept {kept}");
    }

    /// A 144 Hz game under a 66 fps cap keeps 66, not every third frame (48).
    #[test]
    fn the_schedule_holds_the_average_between_refresh_ticks() {
        let mut cap = SoftCap::new(super::super::start::capture_interval(60));
        let kept = (0..144i64).filter(|i| cap.keep(i * 10_000_000 / 144)).count();
        assert!((64..=67).contains(&kept), "kept {kept}");
    }

    /// Frames delivered late in a burst are judged by their own times: the burst keeps the
    /// frames that fell due, not only the first to arrive.
    #[test]
    fn a_late_burst_is_judged_by_frame_time() {
        let mut cap = SoftCap::new(Duration::from_millis(10));
        assert!(cap.keep(0));
        // Six frames 5 ms apart arrive together after a hitch: 10, 20 kept; 5, 15, 25 dropped.
        let kept: Vec<i64> = [5, 10, 15, 20, 25].into_iter().filter(|t| cap.keep(t * MS)).collect();
        assert_eq!(kept, vec![10, 20]);
    }

    #[test]
    fn a_pause_restarts_the_schedule_without_a_burst() {
        let mut cap = SoftCap::new(Duration::from_millis(10));
        assert!(cap.keep(0));
        assert!(cap.keep(1_000 * MS), "first frame after a 1 s pause");
        assert!(!cap.keep(1_005 * MS), "no catch-up burst");
        assert!(cap.keep(1_010 * MS));
    }
}

mod window_exists_tests {
    use super::super::start::{owner_matches, window_owner};

    /// A closed game's handle is not retried as a capture (each try opened a D3D11 device).
    #[test]
    fn a_closed_window_handle_has_no_owner() {
        assert_eq!(window_owner(0), None);
        assert_eq!(window_owner(0x0BAD_F00D), None, "a handle no window has");
        let desktop = unsafe { windows::Win32::UI::WindowsAndMessaging::GetDesktopWindow() };
        assert!(window_owner(desktop.0 as usize).is_some());
    }

    /// The owner is the window's own process, remembered at the first check: a Store game's
    /// frame window (owned by the frame host, pid 100) stays live although the controller
    /// names the hosted game (pid 200); a recycled handle owned by another process does not.
    #[test]
    fn the_first_owner_is_kept_and_a_recycled_handle_fails() {
        let mut first = None;
        assert!(owner_matches(&mut first, 0x1234, 100), "first check: frame host owns the frame");
        assert!(owner_matches(&mut first, 0x1234, 100), "later check, same owner");
        assert!(!owner_matches(&mut first, 0x1234, 200), "same handle, another process: recycled");
        assert!(owner_matches(&mut first, 0x5678, 300), "a new target window starts over");
    }
}
