use super::*;

fn secs(s: f32) -> Duration {
    Duration::from_secs_f32(s)
}

/// Feed `seen` every 100 ms from `from` to `to` seconds; the times (s) a restart was asked.
fn run(r: &mut EncoderRecovery, t0: Instant, from: f32, to: f32, seen: Seen, busy: bool) -> Vec<f32> {
    let mut out = Vec::new();
    let mut t = from;
    while t <= to {
        if r.observe(t0 + secs(t), seen, busy) == Recovery::Restart {
            out.push(t);
        }
        t = ((t + 0.1) * 10.0).round() / 10.0;
    }
    out
}

#[test]
fn restarts_once_the_encoder_error_lasts_past_fifteen_seconds() {
    let t0 = Instant::now();
    let mut r = EncoderRecovery::default();
    assert!(run(&mut r, t0, 0.0, 15.0, Seen::EncoderFailed, false).is_empty(), "not at or before 15 s");
    assert_eq!(run(&mut r, t0, 15.1, 15.1, Seen::EncoderFailed, false), vec![15.1]);
}

#[test]
fn a_broken_error_starts_the_fifteen_seconds_over() {
    let t0 = Instant::now();
    let mut r = EncoderRecovery::default();
    run(&mut r, t0, 0.0, 10.0, Seen::EncoderFailed, false);
    run(&mut r, t0, 10.1, 12.0, Seen::Recording, false); // the engine's own retry worked
    assert!(run(&mut r, t0, 12.1, 27.0, Seen::EncoderFailed, false).is_empty());
    assert_eq!(run(&mut r, t0, 27.1, 27.3, Seen::EncoderFailed, false).len(), 1, "15 s after the error came back");
}

#[test]
fn restarts_never_loop_faster_than_the_backoff() {
    let t0 = Instant::now();
    let mut r = EncoderRecovery::default();
    // The encoder keeps failing in every new engine (2 s of starting each time).
    let mut asked = Vec::new();
    let mut t = 0.0f32;
    while t < 900.0 {
        let seen = if asked.last().is_some_and(|&a: &f32| t < a + 2.0) { Seen::Starting } else { Seen::EncoderFailed };
        if r.observe(t0 + secs(t), seen, false) == Recovery::Restart {
            asked.push(t);
        }
        t = ((t + 0.1) * 10.0).round() / 10.0;
    }
    assert!(asked.len() >= 3, "{asked:?}");
    let gaps: Vec<f32> = asked.windows(2).map(|w| w[1] - w[0]).collect();
    for (n, gap) in gaps.iter().enumerate() {
        let least = RESTART_GAPS[n.min(RESTART_GAPS.len() - 1)].as_secs_f32();
        assert!(*gap >= least - 0.05, "restart {} came {gap} s after the last (least {least}): {asked:?}", n + 2);
    }
    assert!(gaps.last().unwrap() >= &299.9, "settles at one restart per 5 min: {asked:?}");
}

#[test]
fn waits_while_a_save_or_long_recording_is_in_flight() {
    let t0 = Instant::now();
    let mut r = EncoderRecovery::default();
    assert!(run(&mut r, t0, 0.0, 30.0, Seen::EncoderFailed, true).is_empty());
    assert_eq!(run(&mut r, t0, 30.1, 30.1, Seen::EncoderFailed, false), vec![30.1]);
}

#[test]
fn says_recovered_once_recording_returns_and_a_stable_minute_resets_the_backoff() {
    let t0 = Instant::now();
    let mut r = EncoderRecovery::default();
    assert_eq!(run(&mut r, t0, 0.0, 15.1, Seen::EncoderFailed, false), vec![15.1]);
    assert_eq!(r.observe(t0 + secs(16.0), Seen::Starting, false), Recovery::Nothing);
    assert_eq!(r.observe(t0 + secs(18.0), Seen::Recording, false), Recovery::Recovered);
    assert_eq!(r.observe(t0 + secs(18.1), Seen::Recording, false), Recovery::Nothing, "said once");
    run(&mut r, t0, 18.2, 18.0 + STABLE_AFTER.as_secs_f32(), Seen::Recording, false);
    // A new failure much later gets the first restart again, without a 30 s gap.
    let at = 18.0 + STABLE_AFTER.as_secs_f32() + 1.0;
    assert!(run(&mut r, t0, at, at + 15.0, Seen::EncoderFailed, false).is_empty());
    assert_eq!(run(&mut r, t0, at + 15.1, at + 15.1, Seen::EncoderFailed, false).len(), 1);
}

#[test]
fn idle_sessions_never_restart() {
    let t0 = Instant::now();
    let mut r = EncoderRecovery::default();
    assert!(run(&mut r, t0, 0.0, 60.0, Seen::Idle, false).is_empty());
    assert!(run(&mut r, t0, 60.1, 120.0, Seen::Starting, false).is_empty());
}
