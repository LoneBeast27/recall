//! `{"cmd":"stop_capture"}` (final review F-detect-1): the controller's privacy stop (fullscreen
//! Discord after Esc, Never record or Remove on the app being recorded). From that line on no
//! video frame and no audio sample enters the replay ring, nothing more is read from stdin (so
//! no save can start), and the engine quits like an orphan: the saves already asked for finish
//! (`saver_stop::OrphanHold`, the same bounds) instead of being aborted after 3 s. The hello's
//! `stop_capture` says the engine takes it; an engine without it gets a held stop instead.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering::SeqCst};

/// The stop and the stored-frame count (R2-detect-r10-p6-3).
struct Flags {
    stopped: AtomicBool,
    /// GPU frames stored in `latest_gpu` so far ([`next_frame_seq`]).
    frames: AtomicU64,
    /// The frame count when the stop landed: a frame numbered after it may be post-stop.
    stop_seq: AtomicU64,
}

impl Flags {
    const fn new() -> Self {
        Flags { stopped: AtomicBool::new(false), frames: AtomicU64::new(0), stop_seq: AtomicU64::new(u64::MAX) }
    }
}

/// Set once, by the stdin thread, when `stop_capture` arrives.
#[cfg(not(test))]
static FLAGS: Flags = Flags::new();

// Tests run in parallel in one process: per-thread flags keep one test's stop from
// silencing another test's ring.
#[cfg(test)]
thread_local! {
    static FLAGS: Flags = const { Flags::new() };
}

fn flags<R>(f: impl FnOnce(&Flags) -> R) -> R {
    #[cfg(not(test))]
    return f(&FLAGS);
    #[cfg(test)]
    FLAGS.with(f)
}

/// The controller asked to stop capture: frames and samples are dropped from now on.
pub fn request() {
    flags(|f| {
        f.stopped.store(true, SeqCst);
        f.stop_seq.store(f.frames.load(SeqCst), SeqCst);
    });
}

/// Whether `stop_capture` arrived (read for every frame and audio packet).
pub fn stopped() -> bool {
    flags(|f| f.stopped.load(SeqCst))
}

/// The number of a GPU frame being stored in `latest_gpu` (`capture/gpu_path.rs`), taken
/// after its own [`stopped`] check.
pub fn next_frame_seq() -> u64 {
    flags(|f| f.frames.fetch_add(1, SeqCst) + 1)
}

/// A stored frame numbered `seq` was captured before the stop (or there is none): it may be
/// read back (a screenshot, the HUD probe). Checked AFTER the read, so a stop landing between
/// a reader's `stopped()` check and its read still refuses the frame.
pub fn frame_admitted(seq: u64) -> bool {
    flags(|f| !f.stopped.load(SeqCst) || seq <= f.stop_seq.load(SeqCst))
}

/// Undo [`request`] on this test thread.
#[cfg(test)]
pub fn reset_for_test() {
    flags(|f| {
        f.stopped.store(false, SeqCst);
        f.stop_seq.store(u64::MAX, SeqCst);
    });
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

/// R2-detect-r10-p6-3, the race itself by injected order: a reader checked `stopped()` (false),
/// then the stop lands, then the capture thread (past its own check just before) stores a
/// frame, then the reader reads it. The re-check after the read refuses that frame and keeps
/// the one stored before the stop.
#[cfg(test)]
#[test]
fn a_frame_stored_after_the_stop_is_refused_after_the_read() {
    reset_for_test();
    let before = next_frame_seq();
    assert!(!stopped(), "the reader's check passes");
    request(); // the stop lands between the reader's check and its read
    let after = next_frame_seq(); // the capture thread stored one more
    assert!(frame_admitted(before), "a frame stored before the stop may be read");
    assert!(!frame_admitted(after), "the frame read after the stop was stored after it: refused");
    reset_for_test();
}

/// R2-detect-r10-p6-3: where each guard sits. The capture thread stores no frame after the stop
/// (check before `latest_gpu.lock()`, numbered after it); the screenshot (GPU path) and the HUD
/// probe re-check the frame's number after their read, before using it.
#[cfg(test)]
#[test]
fn the_frame_store_and_both_readers_are_guarded_against_the_stop() {
    let order = |src: &str, from: &str, marks: &[&str]| {
        let body = &src[src.find(from).unwrap_or_else(|| panic!("{from}"))..];
        let at: Vec<usize> = marks.iter().map(|m| body.find(m).unwrap_or_else(|| panic!("{from}: no {m}"))).collect();
        assert!(at.windows(2).all(|w| w[0] < w[1]), "{from}: {marks:?} at {at:?}");
    };
    order(include_str!("capture/gpu_path.rs"), "match pipe.render(", &["!crate::stop_capture::stopped()", "self.shared.latest_gpu.lock()", "crate::stop_capture::next_frame_seq()"]);
    order(include_str!("app_tick.rs"), "fn screenshot_tick", &["latest_gpu.lock()", "latest_gpu_seq.load(", "read_rgba(", "frame_admitted(seq)", "shots.submit("]);
    order(include_str!("hud_probe.rs"), "fn grab(shared: &CaptureShared, reader: &mut Reader)", &["latest_gpu.lock()", "latest_gpu_seq.load(", "reader.read(", "frame_admitted(seq)"]);
}
