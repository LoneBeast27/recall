use std::time::{Duration, Instant};

use super::*;

/// Frames delivered in one second of a `hz` desktop through a pacer capped at `cap_fps`.
fn delivered(hz: f64, cap_fps: f64) -> usize {
    let start = Instant::now();
    let mut pacer = Pacer::new(Duration::from_secs_f64(1.0 / cap_fps));
    (0..hz as usize)
        .filter(|i| pacer.offer(start + Duration::from_secs_f64(*i as f64 / hz)) == Pace::Deliver)
        .count()
}

#[test]
fn pacer_keeps_the_cap_on_fast_desktops_and_every_frame_on_slow_ones() {
    let cap = 66.0; // capture_interval(60)
    assert_eq!(delivered(60.0, cap), 60, "a 60 Hz desktop under a 66 fps cap keeps every frame");
    for hz in [144.0, 165.0, 240.0, 500.0] {
        let n = delivered(hz, cap);
        assert!((63..=67).contains(&n), "{hz} Hz desktop gave {n} frames, want about 66");
    }
}

#[test]
fn pacer_skips_with_the_wait_until_the_next_slot() {
    let t0 = Instant::now();
    let mut pacer = Pacer::new(Duration::from_millis(15));
    assert_eq!(pacer.offer(t0), Pace::Deliver);
    assert_eq!(pacer.offer(t0 + Duration::from_millis(5)), Pace::Skip(Duration::from_millis(10)));
    assert_eq!(pacer.offer(t0 + Duration::from_millis(15)), Pace::Deliver);
}

#[test]
fn pacer_restarts_after_an_idle_desktop_instead_of_bursting() {
    let t0 = Instant::now();
    let mut pacer = Pacer::new(Duration::from_millis(15));
    assert_eq!(pacer.offer(t0), Pace::Deliver);
    let back = t0 + Duration::from_secs(2);
    assert_eq!(pacer.offer(back), Pace::Deliver);
    assert!(matches!(pacer.offer(back + Duration::from_millis(1)), Pace::Skip(_)), "no burst of catch-up frames");
}

#[test]
fn format_follows_the_capture_start_format() {
    assert_eq!(duplication_format(true), DXGI_FORMAT_R16G16B16A16_FLOAT);
    assert_eq!(duplication_format(false), DXGI_FORMAT_B8G8R8A8_UNORM);
    assert!(legacy_duplication_fits(false));
    assert!(!legacy_duplication_fits(true), "legacy DuplicateOutput is Bgra8 only");
    assert_eq!(format_label(duplication_format(true)), "Rgba16F");
    assert_eq!(format_label(duplication_format(false)), "Bgra8");
}

#[test]
fn rotated_monitors_stay_on_wgc() {
    use windows::Win32::Graphics::Dxgi::Common::{DXGI_MODE_ROTATION_ROTATE270, DXGI_MODE_ROTATION_ROTATE90};
    assert!(rotation_supported(DXGI_MODE_ROTATION_IDENTITY));
    assert!(rotation_supported(DXGI_MODE_ROTATION_UNSPECIFIED));
    assert!(!rotation_supported(DXGI_MODE_ROTATION_ROTATE90));
    assert!(!rotation_supported(DXGI_MODE_ROTATION_ROTATE270));
}

#[test]
fn errors_map_to_idle_recreate_or_fatal() {
    use windows::Win32::Graphics::Dxgi::{DXGI_ERROR_INVALID_CALL, DXGI_ERROR_SESSION_DISCONNECTED};
    assert_eq!(action_for(DXGI_ERROR_WAIT_TIMEOUT), Action::Idle);
    for code in [DXGI_ERROR_ACCESS_LOST, DXGI_ERROR_ACCESS_DENIED, E_ACCESSDENIED, DXGI_ERROR_INVALID_CALL, DXGI_ERROR_SESSION_DISCONNECTED] {
        assert_eq!(action_for(code), Action::Recreate, "{code:?}");
    }
    for code in [DXGI_ERROR_DEVICE_REMOVED, DXGI_ERROR_DEVICE_RESET, DXGI_ERROR_DEVICE_HUNG] {
        assert_eq!(action_for(code), Action::Fatal, "{code:?}");
    }
}

#[test]
fn backoff_doubles_and_caps() {
    assert_eq!(backoff(0), Duration::from_millis(50));
    assert_eq!(backoff(1), Duration::from_millis(100));
    assert_eq!(backoff(5), Duration::from_millis(1600));
    assert_eq!(backoff(6), Duration::from_millis(2000));
    assert_eq!(backoff(60), Duration::from_millis(2000));
}

#[test]
fn qpc_converts_to_wgc_units() {
    assert_eq!(qpc_to_100ns(10_000_000, 10_000_000), 10_000_000);
    assert_eq!(qpc_to_100ns(3_000_000, 3_000_000), 10_000_000, "1 s at a 3 MHz QPC");
    assert_eq!(qpc_to_100ns(i64::MAX / 2, 10_000_000), i64::MAX / 2, "no overflow");
    assert_eq!(qpc_to_100ns(5, 0), 0);
}

#[test]
fn mouse_only_updates_carry_no_image() {
    assert!(!has_new_image(0, 0, false), "mouse move");
    assert!(!has_new_image(0, 1, false));
    assert!(has_new_image(123, 1, false));
    assert!(has_new_image(0, 0, true), "first frame after a create is the desktop");
}

#[test]
fn acquires_wait_for_the_next_slot() {
    let t0 = Instant::now();
    let mut pacer = Pacer::new(Duration::from_millis(15));
    assert_eq!(pacer.until_due(t0), Duration::ZERO, "nothing delivered yet");
    assert_eq!(pacer.offer(t0), Pace::Deliver);
    assert_eq!(pacer.until_due(t0 + Duration::from_millis(4)), Duration::from_millis(11));
    assert_eq!(pacer.until_due(t0 + Duration::from_millis(20)), Duration::ZERO);
}

#[test]
fn re_create_gives_up_after_five_seconds() {
    let t0 = Instant::now();
    assert!(!should_give_up(t0, t0 + Duration::from_secs(5)));
    assert!(should_give_up(t0, t0 + Duration::from_millis(5001)));
}

#[test]
fn desktop_duplication_is_opt_in_and_off_after_a_failure() {
    assert!(dda_wanted(None, false).is_err(), "default stays WGC");
    assert!(dda_wanted(Some("wgc"), false).is_err());
    assert!(dda_wanted(Some("dda"), false).is_ok());
    assert!(dda_wanted(Some(" DDA "), false).is_ok());
    assert_eq!(dda_wanted(Some("dda"), true), Err("desktop duplication failed earlier this run"));
}

#[test]
fn window_covers_monitor_only_when_it_spans_it() {
    let monitor = Bounds { left: 0, top: 0, right: 2560, bottom: 1440 };
    assert!(covers(monitor, monitor), "borderless fullscreen");
    assert!(covers(Bounds { left: -8, top: -8, right: 2568, bottom: 1448 }, monitor), "overhang");
    assert!(!covers(Bounds { left: 0, top: 0, right: 1920, bottom: 1080 }, monitor), "windowed");
    assert!(!covers(Bounds { left: 0, top: 31, right: 2560, bottom: 1440 }, monitor), "title bar shows the desktop above");
    let second = Bounds { left: 2560, top: 0, right: 5120, bottom: 1440 };
    assert!(!covers(monitor, second), "other monitor");
    assert!(!covers(monitor, Bounds { left: 0, top: 0, right: 0, bottom: 0 }), "empty monitor rect");
}
