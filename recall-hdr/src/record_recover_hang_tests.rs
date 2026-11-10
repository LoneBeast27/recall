//! The hung-engine rule and the 90 s recovery bound, through `EncoderRecovery::step` (the
//! body of `encoder_recover_tick`): `progress` is the engine's encoder heartbeat (bumped
//! every encoder-thread turn, frames or not), or its status line count on an engine that
//! sends none.

use super::*;

/// `step` once a second for `0..=to`: (state, progress, shown) per second; the seconds a
/// restart was asked.
fn tick(to: u64, at: impl Fn(u64) -> (&'static str, u64, bool)) -> Vec<u64> {
    let t0 = Awake::ZERO;
    let mut r = EncoderRecovery::default();
    let up = EngineHealth::Running { pid: 9 };
    (0..=to)
        .filter(|&s| {
            let (state, progress, shown) = at(s);
            r.step(t0 + Duration::from_secs(s), state, up, progress, shown, false) == Recovery::Restart
        })
        .collect()
}

#[test]
fn a_recovering_engine_whose_heartbeat_stopped_gets_exactly_one_prompt_restart() {
    // An NVENC open hung after a TDR: the encoder thread stops beating at second 0.
    assert_eq!(tick(300, |_| ("recovering", 3, true)), [10]);
}

#[test]
fn a_recording_engine_whose_heartbeat_stopped_gets_exactly_one_restart() {
    // Status still says "recording", but the encoder thread stopped at second 20.
    assert_eq!(tick(300, |s| ("recording", s.min(20), true)), [30]);
}

#[test]
fn a_static_game_with_a_beating_encoder_never_restarts() {
    // A paused menu: no frames, but the encoder thread turns every 100 ms.
    assert_eq!(tick(300, |s| ("recording", s * 10, true)), Vec::<u64>::new());
}

#[test]
fn one_real_error_then_recovery_then_a_static_game_for_300_s_never_escalates() {
    let at = |s: u64| (if s < 5 { "recovering" } else { "recording" }, s * 10, true);
    assert_eq!(tick(305, at), Vec::<u64>::new());
}

#[test]
fn the_same_with_a_hang_after_recovery_restarts_exactly_once() {
    // Recovered at 5 s; the encoder thread hangs at 100 s; the restarted engine comes back
    // and hangs again. One restart only: a second hang right after it is not looped on.
    let at = |s: u64| match s {
        0..=4 => ("recovering", s * 10, true),
        5..=100 => ("recording", s * 10, true),
        101..=110 => ("recording", 1_000, true),
        111..=115 => ("starting", 0, true),
        _ => ("recording", 7, true),
    };
    assert_eq!(tick(300, at), [110]);
}

#[test]
fn a_hang_after_a_healthy_minute_gets_its_own_restart() {
    let at = |s: u64| match s {
        0..=10 => ("recording", 3, true), // hung: restart at 10
        11..=14 => ("starting", 0, true),
        15..=100 => ("recording", s * 10, true), // the new engine beats for 85 s
        _ => ("recording", 1_000, true),          // and hangs again at 100
    };
    assert_eq!(tick(200, at), [10, 110]);
}

#[test]
fn a_hidden_game_gets_no_hang_restart() {
    assert_eq!(tick(120, |_| ("recovering", 3, false)), Vec::<u64>::new());
}

#[test]
fn recovering_for_90_s_of_visible_time_counts_as_failed() {
    // The engine beats (it is alive) but no encode succeeds: failed at 90 s visible, then
    // the usual 15 s error grace.
    assert_eq!(tick(120, |s| ("recovering", s * 10, true)), [106]);
}

#[test]
fn the_90_s_bound_counts_visible_time_only() {
    // Hidden from 20 s to 49 s: 19 s seen before, then 90 s reached at 121 s.
    assert_eq!(tick(160, |s| ("recovering", s * 10, !(20..50).contains(&s))), [137]);
    // Minimized the whole time: no bound, as before.
    assert_eq!(tick(600, |s| ("recovering", s * 10, false)), Vec::<u64>::new());
}
