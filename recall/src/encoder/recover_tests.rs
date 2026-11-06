//! Forced failures on the real encoder loop (real NVENC, injected faults, injected saver):
//! a failed encode or open rebuilds the encoder in place, the replay buffer survives, a
//! save in the gap holds the frames already buffered, and a removed device asks for a new
//! capture and resumes on it. Skips without NVENC, like `wobble_tests`.

use std::sync::atomic::{AtomicU32, Ordering::Relaxed};
use std::sync::Arc;
use std::time::{Duration, Instant};

use super::frame_fit::wobble_tests::{nvenc, Capture, Loop, BIG};
use super::EncStats;

/// 60 frames are 1 s of clip.
const SECOND: u64 = 60;

/// Stats with fast retries and a counted capture-restart hook.
fn stats() -> (Arc<EncStats>, Arc<AtomicU32>) {
    let stats = Arc::new(EncStats::default());
    stats.faults.fast.store(true, Relaxed);
    let asked = Arc::new(AtomicU32::new(0));
    let counter = asked.clone();
    let _ = stats.capture_restart.set(Box::new(move || {
        counter.fetch_add(1, Relaxed);
    }));
    (stats, asked)
}

fn wait(mut until: impl FnMut() -> bool) -> bool {
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        if until() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    until()
}

/// CPU frames at 60 fps real time (the loop's retry clock is real).
fn rgba_for(enc: &mut Loop, count: u64) {
    for _ in 0..count {
        enc.rgba(BIG);
        std::thread::sleep(Duration::from_millis(16));
    }
}

#[test]
fn an_encode_error_rebuilds_the_encoder_in_place_and_keeps_the_buffer() {
    let _one = nvenc();
    let (stats, asked) = stats();
    let mut enc = Loop::start_with(stats.clone());
    enc.rgba(BIG);
    if !enc.nvenc_opened() {
        return;
    }
    rgba_for(&mut enc, SECOND);
    stats.faults.encode_errors.store(1, Relaxed);
    rgba_for(&mut enc, 2);
    assert!(wait(|| stats.backend.load(Relaxed) != 1), "the injected encode error did not land");
    // Frames keep coming; the encoder reopens on its own (no capture or engine restart).
    rgba_for(&mut enc, SECOND);
    assert!(wait(|| stats.backend.load(Relaxed) == 1), "encoder never came back: {:?}", enc.error());
    let secs = enc.clip_after_settling();
    eprintln!("after an encode error: clip {secs:.2} s");
    assert!(secs > 1.6, "the clip lost the buffer from before the error: {secs:.2} s of ~2 s");
    assert_eq!(asked.load(Relaxed), 0, "one encode error needs no new capture");
    assert_eq!(enc.error(), None);
}

#[test]
fn a_save_in_the_encoder_gap_holds_the_frames_already_buffered() {
    let _one = nvenc();
    let (stats, _) = stats();
    let mut enc = Loop::start_with(stats.clone());
    enc.rgba(BIG);
    if !enc.nvenc_opened() {
        return;
    }
    rgba_for(&mut enc, SECOND);
    // The encode fails and every reopen fails too: the gap lasts.
    stats.faults.open_errors.store(1_000, Relaxed);
    stats.faults.encode_errors.store(1, Relaxed);
    rgba_for(&mut enc, 10);
    assert!(wait(|| stats.backend.load(Relaxed) != 1));
    let secs = enc.clip_after_settling();
    eprintln!("save in the gap: clip {secs:.2} s");
    assert!(secs > 0.9, "a save in the gap holds {secs:.2} s of the 1 s buffered");
}

#[test]
fn a_removed_device_asks_for_a_new_capture_and_resumes_on_it_with_the_buffer() {
    let _one = nvenc();
    let Some(mut old) = Capture::new() else { return };
    let (stats, asked) = stats();
    let mut enc = Loop::start_with(stats.clone());
    old.frames(&mut enc, BIG, 1);
    if !enc.nvenc_opened() {
        return;
    }
    for _ in 0..SECOND {
        old.frames(&mut enc, BIG, 1);
        std::thread::sleep(Duration::from_millis(16));
    }
    // TDR: the device is gone; NVENC cannot open on its frames any more.
    *stats.faults.dead_pool.lock().unwrap() = Some(old.pool(BIG));
    stats.faults.encode_errors.store(1, Relaxed);
    let restart = wait(|| {
        old.frames(&mut enc, BIG, 1);
        asked.load(Relaxed) > 0
    });
    assert!(restart, "the encoder never asked for a new capture device: {:?}", enc.error());
    let gap = enc.clip_after_settling();
    eprintln!("removed device, save in the gap: clip {gap:.2} s");
    assert!(gap > 0.9, "a save in the gap holds {gap:.2} s");
    assert!(!stats.gpu_refused.load(Relaxed), "the recovery gave up GPU frames before the new device");
    // The capture restart brings a new device and frames pool.
    let Some(mut new) = Capture::new() else { return };
    for _ in 0..SECOND {
        new.frames(&mut enc, BIG, 1);
        std::thread::sleep(Duration::from_millis(16));
    }
    assert!(wait(|| stats.backend.load(Relaxed) == 1), "no encoder on the new device: {:?}", enc.error());
    let secs = enc.clip_after_settling();
    eprintln!("removed device, after the new capture: clip {secs:.2} s");
    assert!(secs > 1.6, "the buffer from the old device is gone: {secs:.2} s of ~2 s");
    assert_eq!(enc.error(), None);
}

#[test]
fn a_cpu_fallback_reports_degraded_and_the_gpu_path_comes_back() {
    let _one = nvenc();
    let Some(mut capture) = Capture::new() else { return };
    let (stats, _) = stats();
    // The first GPU open is refused (as NVENC refusing D3D11 frames does).
    stats.faults.open_errors.store(1, Relaxed);
    let mut enc = Loop::start_with(stats.clone());
    capture.frames(&mut enc, BIG, 1);
    assert!(wait(|| stats.gpu_refused.load(Relaxed)), "GPU frames were not refused");
    rgba_for(&mut enc, 10);
    assert!(wait(|| stats.backend.load(Relaxed) == 1), "no encoder on CPU copies: {:?}", enc.error());
    assert_eq!(stats.label(), "degraded", "CPU copies are not plain recording");
    // The GPU path is tried again on its own (1 min, here 1 s): capture is told.
    assert!(wait(|| !stats.gpu_refused.load(Relaxed)), "the CPU fallback latched");
    assert!(stats.gpu_retry.load(Relaxed), "capture was not asked to try the GPU path");
    for _ in 0..10 {
        capture.frames(&mut enc, BIG, 1);
        std::thread::sleep(Duration::from_millis(16));
    }
    assert!(wait(|| stats.label() == "h264_nvenc"), "label {} after the GPU came back", stats.label());
}
