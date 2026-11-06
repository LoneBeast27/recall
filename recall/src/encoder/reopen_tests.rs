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
fn a_retry_no_frame_came_for_counts_as_failed() {
    let t0 = Instant::now();
    let mut r = Reopen::default();
    r.failed(SIZE, t0);
    assert_eq!(r.stalled(t0 + secs(3.9)), None, "due at 2 s, overdue after 4 s");
    assert_eq!(r.stalled(t0 + secs(4.0)), Some(AfterFail::Wait(secs(5.0))));
    assert_eq!(r.reopen_failures(), 1, "the plan moves on without frames");
}

#[test]
fn status_hands_off_to_the_controller_only_when_the_engine_plan_is_spent() {
    let t0 = Instant::now();
    let mut r = Reopen::default();
    r.failed(SIZE, t0);
    for n in 1..HAND_OFF_AFTER {
        r.failed(SIZE, t0 + secs(n as f32));
        assert!(!r.handed_off(t0 + secs(n as f32)), "failure {n}: still the engine's turn");
    }
    // The capture restart (3) and the CPU fallback (4) came first.
    const { assert!(RESTART_EVERY < CPU_FALLBACK_AFTER && CPU_FALLBACK_AFTER < HAND_OFF_AFTER) };
    r.failed(SIZE, t0 + secs(10.0));
    assert!(r.handed_off(t0 + secs(10.0)));
    // Or by age, with no encoder for a minute.
    let mut slow = Reopen::default();
    slow.failed(SIZE, t0);
    assert!(!slow.handed_off(t0 + HAND_OFF_AGE - secs(0.1)));
    assert!(slow.handed_off(t0 + HAND_OFF_AGE));
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
