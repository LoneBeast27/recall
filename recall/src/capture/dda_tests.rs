//! Headless check on the real desktop (no window, no audio): duplicate the primary monitor for
//! a few seconds in each format and report what arrived. Ignored by default (needs a display):
//! `cargo test -p recall dda_headless -- --ignored --nocapture`.

use std::sync::atomic::Ordering::Relaxed;
use std::time::Duration;

use windows::Win32::Graphics::Gdi::HMONITOR;

/// What one run delivered: (delivered, skipped, size, format, newest CPU frame (w, h, bytes)).
type Report = (u64, u64, (u32, u32), &'static str, Option<(u32, u32, usize)>);

fn run(fp16: bool, secs: f64) -> Report {
    let monitor = windows_capture::monitor::Monitor::primary().expect("primary monitor");
    let shared = super::super::CaptureShared::new();
    shared.fp16.store(fp16, Relaxed);
    let control = super::start(HMONITOR(monitor.as_raw_hmonitor()), shared.clone(), super::super::start::capture_interval(60), None)
        .expect("dda starts on the primary monitor");
    std::thread::sleep(Duration::from_secs_f64(secs));
    let stats = control.stats.clone();
    control.stop().expect("dda stops");
    println!(
        "[dda check] fp16={fp16} acquired={} idle_timeouts={} no_image={} recreated={}",
        stats.acquired.load(Relaxed),
        stats.idle.load(Relaxed),
        stats.no_image.load(Relaxed),
        stats.recreated.load(Relaxed)
    );
    let latest = shared.latest.lock().unwrap().take().map(|f| (f.width, f.height, f.bytes.len()));
    let size = *stats.size.lock().unwrap();
    let format = *stats.format.lock().unwrap();
    (stats.delivered.load(Relaxed), stats.skipped.load(Relaxed), size, format, latest)
}

#[test]
#[ignore = "needs a display; run through the gate with --ignored and RECALL_HW_TESTS=1"]
fn dda_headless_primary_monitor() {
    if !crate::hw_gate::enabled("dda_headless_primary_monitor") {
        return;
    }
    // RECALL_DDA_CHECK_SECS lengthens each run (an idle desktop changes at least once a
    // minute: the taskbar clock).
    let secs = std::env::var("RECALL_DDA_CHECK_SECS").ok().and_then(|v| v.parse().ok()).unwrap_or(3.0);
    for fp16 in [true, false] {
        let (delivered, skipped, size, format, latest) = run(fp16, secs);
        println!(
            "[dda check] fp16={fp16} format={format} size={}x{} delivered={delivered} skipped={skipped} cpu_frame={latest:?}",
            size.0, size.1
        );
        // Desktop Duplication sends a frame only when the screen changes: an idle desktop
        // can give none in the window (the pacer then repeats the last frame, as on WGC idle).
        if delivered == 0 {
            println!("[dda check] fp16={fp16}: desktop idle, no frame in {secs} s");
            continue;
        }
        assert_eq!(format, if fp16 { "Rgba16F" } else { "Bgra8" });
        if fp16 {
            // No encoder sink in a test: fp16 frames take the CPU path, 8 bytes a pixel.
            let (w, h, len) = latest.expect("a CPU frame reached the shared slot");
            assert_eq!((w, h), size);
            assert_eq!(len, w as usize * h as usize * 8);
        } else {
            assert!(latest.is_none(), "Bgra8 frames never take the CPU path");
        }
    }
}

/// The follow glue the capture thread runs every `FOLLOW_CHECK`: holds never ask for a
/// capture restart (the encoder and replay buffer keep running), resuming takes the next
/// frame, and only a `Restart` verdict sets `CaptureShared::restart`.
#[test]
fn holding_never_restarts_and_resuming_takes_the_next_frame() {
    use super::{apply_follow, Applied, Follow, Hold};
    let shared = super::super::CaptureShared::new();
    let mut held = None;
    assert_eq!(apply_follow(&shared, &mut held, Follow::Deliver), Applied::Same);
    for why in [Hold::Covered, Hold::Minimized, Hold::Away, Hold::Minimized] {
        assert_eq!(apply_follow(&shared, &mut held, Follow::Hold(why)), Applied::Same);
        assert_eq!(held, Some(why));
    }
    assert!(!shared.restart.load(Relaxed), "a hold never restarts capture");
    assert_eq!(apply_follow(&shared, &mut held, Follow::Deliver), Applied::Resumed, "back on top");
    assert_eq!(held, None);
    assert!(shared.restart_reason().is_none(), "no restart after a whole alt-tab");
    assert_eq!(apply_follow(&shared, &mut held, Follow::Restart("moved")), Applied::Ended);
    assert!(shared.restart_reason().is_some(), "a real monitor change restarts");
}
