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

mod hdr_flip_tests {
    use std::sync::atomic::Ordering::Relaxed;

    use super::super::{CaptureShared, MODE_PASSTHROUGH, MODE_TONEMAP};

    /// U4: Windows HDR turns on mid-game (Win+Alt+B, a game's own switch). On a display that
    /// can do HDR the capture already runs fp16, so the engine's periodic check finds nothing
    /// to restart: same capture, same D3D11 device, the encoder and replay buffer keep going.
    #[test]
    fn hdr_turning_on_mid_game_keeps_the_capture_on_an_hdr_capable_display() {
        let shared = CaptureShared::new();
        shared.hdr_capable.store(true, Relaxed);
        shared.mode.store(MODE_PASSTHROUGH, Relaxed); // the desktop is SDR at the start
        let _ = shared.start_format();
        assert!(shared.fp16.load(Relaxed), "an HDR-capable display starts in Rgba16F");
        shared.mode.store(MODE_TONEMAP, Relaxed); // HDR turned on
        assert_eq!(shared.restart_reason(), None, "no capture restart, so no encoder reopen");
    }

    /// A display that cannot do HDR keeps the cheaper Bgra8 capture; if HDR still turns on
    /// (the capability probe failed), the restart stays and says it was HDR.
    #[test]
    fn hdr_turning_on_on_an_sdr_only_display_still_restarts_and_says_why() {
        let shared = CaptureShared::new();
        shared.mode.store(MODE_PASSTHROUGH, Relaxed);
        let _ = shared.start_format();
        assert!(!shared.fp16.load(Relaxed), "SDR-only display keeps Bgra8");
        shared.mode.store(MODE_TONEMAP, Relaxed);
        assert_eq!(shared.restart_reason(), Some(super::super::format::HDR_TURNED_ON));
    }

    /// Learning that the display can do HDR does not restart an SDR capture by itself; only
    /// the next start picks fp16.
    #[test]
    fn hdr_capability_alone_never_restarts_a_running_capture() {
        let shared = CaptureShared::new();
        shared.mode.store(MODE_PASSTHROUGH, Relaxed);
        let _ = shared.start_format();
        shared.hdr_capable.store(true, Relaxed);
        assert_eq!(shared.restart_reason(), None);
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
