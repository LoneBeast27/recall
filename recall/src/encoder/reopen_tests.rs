use super::*;

const SIZE: Input = Input { width: 2560, height: 1440, pool: Some(7) };

fn secs(s: f32) -> Duration {
    Duration::from_secs_f32(s)
}

#[test]
fn failed_input_is_retried_on_the_backoff_schedule() {
    let t0 = Instant::now();
    let mut r = Reopen::default();
    assert!(r.may_open(SIZE, t0), "nothing failed yet");
    assert_eq!(r.failed(SIZE, t0), AfterFail::Wait(secs(2.0)), "the encode error itself");
    assert!(!r.may_open(SIZE, t0 + secs(1.9)), "not before 2 s");
    assert!(r.may_open(SIZE, t0 + secs(2.0)), "the failed size opens again after 2 s");
    // Each failed reopen waits longer: 5 s, 10 s, then every 30 s.
    let mut at = t0 + secs(2.0);
    for (n, wait) in [(1, 5.0), (2, 10.0), (3, 30.0), (4, 30.0), (5, 30.0)] {
        let after = r.failed(SIZE, at);
        let expect = if n % RESTART_EVERY == 0 { AfterFail::RestartCapture(secs(wait)) } else { AfterFail::Wait(secs(wait)) };
        assert_eq!(after, expect, "reopen failure {n}");
        assert_eq!(r.reopen_failures(), n);
        assert!(!r.may_open(SIZE, at + secs(wait) - secs(0.1)), "failure {n}: too early");
        assert!(r.may_open(SIZE, at + secs(wait)), "failure {n}: due");
        at += secs(wait);
    }
}

#[test]
fn every_third_failed_reopen_asks_for_a_capture_restart() {
    let t0 = Instant::now();
    let mut r = Reopen::default();
    let mut asked = Vec::new();
    for n in 0..=9u32 {
        if let AfterFail::RestartCapture(_) = r.failed(SIZE, t0 + secs(n as f32 * 30.0)) {
            asked.push(n);
        }
    }
    assert_eq!(asked, vec![3, 6, 9], "the first encode error is not a reopen");
}

#[test]
fn a_new_pool_or_size_is_tried_at_once() {
    let t0 = Instant::now();
    let mut r = Reopen::default();
    r.failed(SIZE, t0);
    // A capture restart brings a new device and frames pool: no wait for it.
    assert!(r.may_open(Input { pool: Some(8), ..SIZE }, t0));
    assert!(r.may_open(Input { width: 1920, height: 1080, ..SIZE }, t0));
    assert!(!r.may_open(SIZE, t0));
}

#[test]
fn an_encoder_that_fails_right_after_reopening_keeps_backing_off() {
    let t0 = Instant::now();
    let mut r = Reopen::default();
    r.failed(SIZE, t0);
    r.opened(t0 + secs(2.0));
    assert!(r.may_open(SIZE, t0 + secs(2.0)));
    // Opens, then fails on its first frames: no 2 s loop, the episode goes on.
    assert_eq!(r.failed(SIZE, t0 + secs(2.1)), AfterFail::Wait(secs(5.0)));
    assert!(r.recovering());
}

#[test]
fn a_minute_of_clean_encoding_ends_the_episode() {
    let t0 = Instant::now();
    let mut r = Reopen::default();
    r.failed(SIZE, t0);
    r.failed(SIZE, t0 + secs(2.0));
    r.opened(t0 + secs(7.0));
    r.encoded(t0 + secs(30.0));
    assert!(r.recovering(), "not stable yet");
    r.encoded(t0 + secs(7.0) + STABLE_AFTER);
    assert!(!r.recovering());
    assert_eq!(r.failed(SIZE, t0 + secs(100.0)), AfterFail::Wait(secs(2.0)), "a later error starts over at 2 s");
}

#[test]
fn the_live_helpers_ask_capture_for_a_restart_and_keep_gpu_frames_while_recovering() {
    use std::sync::atomic::{AtomicU32, Ordering::Relaxed};
    use std::sync::Arc;
    let stats = super::super::EncStats::default();
    let asked = Arc::new(AtomicU32::new(0));
    let counter = asked.clone();
    let _ = stats.capture_restart.set(Box::new(move || {
        counter.fetch_add(1, Relaxed);
    }));
    let mut r = Reopen::default();
    encode_failed(&mut r, SIZE, &stats, false); // DXGI device removed mid-recording
    for _ in 0..3 {
        open_failed(&mut r, SIZE, true, &stats, false);
    }
    assert_eq!(asked.load(Relaxed), 1, "third failed reopen asks for a new capture device");
    assert!(!stats.gpu_refused.load(Relaxed), "a recovery does not give up GPU frames yet");
    for _ in 0..3 {
        open_failed(&mut r, SIZE, true, &stats, false);
    }
    assert_eq!(asked.load(Relaxed), 2);
    assert!(stats.gpu_refused.load(Relaxed), "still failing after a capture restart: CPU copies");
}

#[test]
fn a_first_gpu_open_refusal_still_switches_to_cpu_at_once() {
    let stats = super::super::EncStats::default();
    let mut r = Reopen::default();
    open_failed(&mut r, SIZE, true, &stats, false);
    assert!(stats.gpu_refused.load(std::sync::atomic::Ordering::Relaxed));
    assert!(!r.recovering(), "the CPU frames that follow open at once");
}

#[test]
fn status_hands_off_to_the_controller_only_when_the_engine_plan_is_spent() {
    let t0 = Instant::now();
    let mut r = Reopen::default();
    r.failed(SIZE, t0);
    for n in 1..HAND_OFF_AFTER {
        r.failed(SIZE, t0 + secs(n as f32));
        assert!(!r.handed_off(), "failure {n}: still the engine's turn");
    }
    // The capture restart (3) and the CPU fallback (4) came first.
    const { assert!(RESTART_EVERY < CPU_FALLBACK_AFTER && CPU_FALLBACK_AFTER < HAND_OFF_AFTER) };
    r.failed(SIZE, t0 + secs(10.0));
    assert!(r.handed_off());
    assert_eq!(r.status(), Some(3));
}

#[test]
fn the_gpu_path_is_tried_again_on_its_own_backoff() {
    let t0 = Instant::now();
    let mut p = GpuProbe::default();
    assert!(!p.due(t0 + secs(3600.0)), "nothing refused yet");
    p.refused(t0);
    assert!(!p.due(t0 + secs(59.0)));
    assert!(p.due(t0 + secs(60.0)));
    assert!(!p.due(t0 + secs(61.0)), "once per try");
    p.refused(t0 + secs(61.0));
    assert!(!p.due(t0 + secs(61.0 + 299.0)));
    assert!(p.due(t0 + secs(61.0 + 300.0)), "then 5 min");
    p.gpu_ok();
    p.refused(t0 + secs(1000.0));
    assert!(p.due(t0 + secs(1060.0)), "working again: back to 1 min");
}

#[test]
fn an_unanswered_gpu_try_counts_as_refused_and_the_next_try_still_comes() {
    let t0 = Instant::now();
    let mut p = GpuProbe::default();
    p.refused(t0);
    assert!(p.due(t0 + secs(60.0)), "the 1 min try");
    assert!(!p.unanswered(t0 + secs(60.0) + PROBE_VERDICT - secs(0.1)));
    assert!(p.unanswered(t0 + secs(60.0) + PROBE_VERDICT), "no GPU frame came back to answer it");
    assert!(!p.unanswered(t0 + secs(200.0)), "once");
    let lapsed = t0 + secs(60.0) + PROBE_VERDICT;
    p.refused(lapsed);
    assert!(!p.due(lapsed + secs(299.0)));
    assert!(p.due(lapsed + secs(300.0)), "the 5 min try still comes");
    // An answered try (GPU frames opened) clears the wait.
    p.gpu_ok();
    p.refused(t0);
    assert!(p.due(t0 + secs(60.0)));
    p.gpu_ok();
    assert!(!p.unanswered(t0 + secs(1000.0)));
}

#[test]
fn an_open_encoder_with_no_frames_for_5_minutes_never_reopens_or_asks_for_a_capture() {
    // A static game (VFR, no WGC repeats), also while a long recording runs: no frame,
    // no failure, so nothing to reopen, no capture restart, no hand-off to the controller.
    let t0 = Instant::now();
    let mut r = Reopen::default();
    r.opened(t0);
    for s in 0..=300 {
        let now = t0 + secs(s as f32);
        assert_eq!(r.status(), None, "{s} s");
        assert!(!r.recovering() && !r.handed_off() && r.may_open(SIZE, now), "{s} s");
    }
}

#[test]
fn a_recovery_with_no_frames_for_2_minutes_never_escalates_but_real_errors_still_do() {
    // A static game (a paused menu: VFR, no WGC repeats) right after an encode error: no
    // frame comes to retry the encoder. That is no evidence against capture, so nothing
    // escalates: no capture restart, no `failed` for the controller (capture-watchdog-v2
    // restarts capture only on evidence too).
    let t0 = Instant::now();
    let mut r = Reopen::default();
    r.failed(SIZE, t0);
    // The loop's per-turn check (`Live::tick`) is `status`; nothing there counts time.
    for tenth in 0..=1200u32 {
        assert_eq!(r.status(), Some(4), "{:.1} s: status must stay `recovering`", tenth as f32 / 10.0);
    }
    assert_eq!(r.reopen_failures(), 0, "no frame, no failure: no capture restart either");
    assert!(r.may_open(SIZE, t0 + secs(120.0)), "the first frame that comes is tried at once");
    // The game moves again and every reopen really fails: the plan escalates as before.
    let back = t0 + secs(121.0);
    let afters: Vec<AfterFail> = (1..=HAND_OFF_AFTER).map(|n| r.failed(SIZE, back + secs(n as f32))).collect();
    assert!(matches!(afters[RESTART_EVERY as usize - 1], AfterFail::RestartCapture(_)), "{afters:?}");
    assert_eq!(r.status(), Some(3), "5 real failed reopens hand off to the controller");
}
