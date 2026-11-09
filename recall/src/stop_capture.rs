//! `{"cmd":"stop_capture"}` (final review F-detect-1): the controller's privacy stop (fullscreen
//! Discord after Esc, Never record or Remove on the app being recorded). From that line on no
//! video frame and no audio sample enters the replay ring, nothing more is read from stdin (so
//! no save can start), and the engine quits like an orphan: the saves already asked for finish
//! (`saver_stop::OrphanHold`, the same bounds) instead of being aborted after 3 s. The hello's
//! `stop_capture` says the engine takes it; an engine without it gets a held stop instead.

#[cfg(not(test))]
use std::sync::atomic::{AtomicBool, Ordering::SeqCst};

/// Set once, by the stdin thread, when `stop_capture` arrives.
#[cfg(not(test))]
static STOPPED: AtomicBool = AtomicBool::new(false);

// Tests run in parallel in one process: a per-thread flag keeps one test's stop from
// silencing another test's ring.
#[cfg(test)]
thread_local! {
    static STOPPED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// The controller asked to stop capture: frames and samples are dropped from now on.
pub fn request() {
    #[cfg(not(test))]
    STOPPED.store(true, SeqCst);
    #[cfg(test)]
    STOPPED.with(|s| s.set(true));
}

/// Whether `stop_capture` arrived (read for every frame and audio packet).
pub fn stopped() -> bool {
    #[cfg(not(test))]
    return STOPPED.load(SeqCst);
    #[cfg(test)]
    STOPPED.with(|s| s.get())
}

/// Undo [`request`] on this test thread.
#[cfg(test)]
pub fn reset_for_test() {
    STOPPED.with(|s| s.set(false));
}

#[cfg(all(test, feature = "encode"))]
mod tests {
    use super::*;
    use crate::audio::TrackRing;

    #[test]
    fn after_stop_capture_no_audio_enters_the_ring() {
        reset_for_test();
        let ring = TrackRing::for_test("game", 2);
        ring.push_for_test(&[0.5; 960]);
        assert!(ring.snapshot(0.01).iter().all(|s| *s == 0.5), "recorded before the stop");
        request();
        ring.push_for_test(&[0.25; 960]);
        assert!(ring.snapshot(0.01).iter().all(|s| *s == 0.5), "the newest samples are still the old ones: nothing entered after it");
        reset_for_test();
    }

    #[test]
    fn the_hello_says_the_engine_takes_it() {
        let hello = crate::protocol::Outbound::Hello { version: "0".into(), monitors: Vec::new(), live_sources: true, live_source_fields: true, stop_capture: true };
        assert_eq!(serde_json::to_value(&hello).unwrap()["stop_capture"], true, "recall-hdr `supervisor_release::STOP_CAPTURE_CAP`");
        let cmd: crate::protocol::Command = serde_json::from_str(r#"{"cmd":"stop_capture"}"#).unwrap();
        assert!(matches!(cmd, crate::protocol::Command::StopCapture));
    }
}

/// Gate p5-2: the CPU path's encode checks the stop itself (the redraw's `quit` check can lag
/// a tick), before a frame is read back for the encoder.
#[cfg(test)]
#[test]
fn the_cpu_encode_tick_checks_the_stop_before_encoding() {
    let src = include_str!("app_tick.rs");
    let start = src.find("fn encode_tick").expect("encode_tick");
    let body = &src[start..];
    let stop = body.find("crate::stop_capture::stopped()").expect("encode_tick checks stop_capture");
    let encode = body.find("gpu.encode_frame()").expect("encode_frame");
    assert!(stop < encode, "the stop is checked before a frame is encoded");
}

/// R2-detect-r10-p5-2, both paths: `screenshot_tick` checks the stop before it reads a frame
/// (`latest_gpu` on the GPU path, `encode_frame` on the CPU path).
#[cfg(test)]
#[test]
fn the_screenshot_tick_checks_the_stop_before_reading_a_frame() {
    let src = include_str!("app_tick.rs");
    let body = &src[src.find("fn screenshot_tick").expect("screenshot_tick")..];
    let body = &body[..body.find("\n    }\n").or_else(|| body.find("\r\n    }\r\n")).expect("end")];
    let stop = body.find("crate::stop_capture::stopped()").expect("screenshot_tick checks stop_capture");
    assert!(stop < body.find("latest_gpu.lock()").expect("GPU path"), "before the GPU path's frame");
    assert!(stop < body.find(".encode_frame()").expect("CPU path"), "before the CPU path's frame");
}

/// R2-detect-r10-p5-2, the GPU path run for real: a screenshot pending at the stop fails
/// ("capture stopped"), it never reads `latest_gpu`.
#[cfg(all(test, feature = "encode"))]
#[test]
fn a_screenshot_pending_at_the_stop_is_not_saved() {
    reset_for_test();
    crate::protocol::test_emits::install();
    let out = std::env::temp_dir().join(format!("recall-shot-stop-{}", std::process::id()));
    let shots = crate::screenshot::spawn(out.to_string_lossy().into_owned(), true);
    let mut app = crate::App { shots: Some(shots.clone()), shared: Some(crate::capture::CaptureShared::new()), ..Default::default() };
    shots.request();
    request();
    app.screenshot_tick();
    reset_for_test();
    let failed = crate::protocol::test_emits::of("screenshot_failed");
    assert!(failed.len() == 1 && failed[0].contains("capture stopped"), "{failed:?}");
    assert!(crate::protocol::test_emits::of("screenshot").is_empty() && !out.exists(), "nothing written");
}
