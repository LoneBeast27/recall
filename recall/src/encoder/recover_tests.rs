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

/// Feed CPU frames until `until` holds (at most `secs` seconds of them).
fn rgba_until(enc: &mut Loop, secs: u64, mut until: impl FnMut() -> bool) -> bool {
    let deadline = Instant::now() + Duration::from_secs(secs);
    while Instant::now() < deadline {
        if until() {
            return true;
        }
        rgba_for(enc, 1);
    }
    until()
}

#[test]
fn a_gpu_retry_that_gets_no_gpu_frame_does_not_stop_the_later_retries() {
    let _one = nvenc();
    let Some(mut capture) = Capture::new() else { return };
    let (stats, _) = stats();
    stats.faults.open_errors.store(1, Relaxed);
    let mut enc = Loop::start_with(stats.clone());
    capture.frames(&mut enc, BIG, 1);
    if !wait(|| stats.gpu_refused.load(Relaxed)) {
        return crate::hw_skip::skipped("GPU frames were never refused (no NVENC?)");
    }
    // The first retry (1 min, here 1 s) comes; capture never answers it with a GPU frame
    // (its pipeline did not start again). CPU copies keep recording.
    assert!(rgba_until(&mut enc, 5, || stats.gpu_retry.swap(false, Relaxed)), "no first GPU retry");
    // The unanswered retry counts as refused: degraded again, and the schedule goes on.
    assert!(rgba_until(&mut enc, 5, || stats.gpu_refused.load(Relaxed)), "the unanswered retry is never counted: label {}", stats.label());
    assert_eq!(stats.label(), "degraded");
    assert!(rgba_until(&mut enc, 10, || stats.gpu_retry.load(Relaxed)), "the later retry (5 min, here 5 s) never came");
}

#[test]
fn a_switch_from_gpu_frames_to_cpu_copies_keeps_the_buffer() {
    // NVENC writes the same parameter sets for D3D11 and RGB0 input (headers_tests), so
    // the replay joins across the switch instead of cutting at it.
    let _one = nvenc();
    let Some(mut capture) = Capture::new() else { return };
    let (stats, _) = stats();
    let mut enc = Loop::start_with(stats.clone());
    capture.frames(&mut enc, BIG, 1);
    if !enc.nvenc_opened() {
        return;
    }
    for _ in 0..SECOND {
        capture.frames(&mut enc, BIG, 1);
        std::thread::sleep(Duration::from_millis(16));
    }
    rgba_for(&mut enc, SECOND);
    let secs = enc.clip_after_settling();
    eprintln!("gpu -> cpu switch: clip {secs:.2} s");
    assert!(secs > 1.6, "the save cut at the GPU -> CPU switch: {secs:.2} s of ~2 s");
}

/// Merge gate 11-07 #3 (P1): the first encode error told the long recording the encoder
/// had failed, before the in-place rebuild, so one transient NVENC error ended it. Only a
/// spent recovery (status `failed`) ends it now.
#[test]
fn one_encode_error_keeps_a_long_recording_and_a_spent_recovery_ends_it() {
    use crate::longrec::tap::{failed_generation, generation_now};
    let _one = nvenc();
    let (stats, _) = stats();
    let mut enc = Loop::start_with(stats.clone());
    enc.rgba(BIG);
    if !enc.nvenc_opened() {
        return;
    }
    rgba_for(&mut enc, 10);
    // Writer tests use generations from 1e6 up; the live loop's are small.
    let failed_since = |from: u64| failed_generation().is_some_and(|g| g >= from && g < 1_000_000);
    let first = generation_now();
    stats.faults.encode_errors.store(1, Relaxed);
    rgba_for(&mut enc, 2);
    assert!(wait(|| stats.backend.load(Relaxed) != 1), "the injected encode error did not land");
    rgba_for(&mut enc, SECOND / 2);
    assert!(wait(|| stats.backend.load(Relaxed) == 1), "encoder never came back: {:?}", enc.error());
    assert!(!failed_since(first), "one encode error ended the long recording (generation {:?})", failed_generation());
    // Now every reopen fails: after the recovery's reopens (fast delays) it is spent.
    let before = generation_now();
    stats.faults.open_errors.store(1_000, Relaxed);
    stats.faults.encode_errors.store(1, Relaxed);
    let spent = wait(|| {
        enc.rgba(BIG);
        std::thread::sleep(Duration::from_millis(16));
        failed_since(before)
    });
    stats.faults.open_errors.store(0, Relaxed);
    assert!(spent, "a spent recovery did not end the long recording: backend {}", stats.backend.load(Relaxed));
    assert_eq!(stats.label(), "failed");
}

#[test]
fn the_encoder_thread_beats_every_turn_without_frames_and_stops_while_an_open_hangs() {
    // No NVENC needed: the beat is the loop's, frames or not.
    let stats = Arc::new(EncStats::default());
    let mut enc = Loop::start_with(stats.clone());
    std::thread::sleep(Duration::from_millis(1_000));
    let idle = stats.heartbeat.load(Relaxed);
    assert!(idle >= 5, "a static game (no frames) must keep the beat: {idle} turns in 1 s");
    // An NVENC open hung after a TDR: the beat stops while it hangs, then goes on.
    stats.faults.open_hang_ms.store(1_500, Relaxed);
    enc.rgba(BIG);
    std::thread::sleep(Duration::from_millis(300));
    let hung = stats.heartbeat.load(Relaxed);
    std::thread::sleep(Duration::from_millis(800));
    assert_eq!(stats.heartbeat.load(Relaxed), hung, "the beat moved during a hung open");
    std::thread::sleep(Duration::from_millis(1_200));
    assert!(stats.heartbeat.load(Relaxed) > hung, "the beat did not come back after the open");
}

#[test]
fn an_open_in_progress_is_flagged_while_it_runs() {
    // No NVENC needed: the flag wraps the open, whatever it answers.
    let stats = Arc::new(EncStats::default());
    let mut enc = Loop::start_with(stats.clone());
    std::thread::sleep(Duration::from_millis(200));
    assert!(!stats.opening.load(Relaxed));
    stats.faults.open_hang_ms.store(1_000, Relaxed);
    enc.rgba(BIG);
    std::thread::sleep(Duration::from_millis(400));
    assert!(stats.opening.load(Relaxed), "a hung open is not flagged as an open in progress");
    std::thread::sleep(Duration::from_millis(1_500));
    assert!(!stats.opening.load(Relaxed), "the flag outlived the open");
}
