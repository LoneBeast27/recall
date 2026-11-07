//! What may hold a hang restart or the 90 s bound back, the open-in-progress hang clock,
//! and giving up after a second hang (`EncoderRecovery::step_held`, the body of
//! `encoder_recover_tick`).

use super::*;

/// One look a second `0..=to`: (state, progress, opening, hold) per second, game shown,
/// target "game-a"; (second, outcome) for every Restart / GaveUp. The engine is not
/// replaced here, so windows end before a second stall of the same engine would count.
fn tick(to: u64, at: impl Fn(u64) -> (&'static str, u64, bool, Hold)) -> Vec<(u64, Recovery)> {
    let t0 = Instant::now();
    let mut r = EncoderRecovery::default();
    let up = EngineHealth::Running { pid: 9 };
    (0..=to)
        .filter_map(|s| {
            let (state, progress, opening, hold) = at(s);
            let out = r.step_held(t0 + Duration::from_secs(s), state, up, progress, opening, true, hold, "game-a");
            matches!(out, Recovery::Restart | Recovery::GaveUp).then_some((s, out))
        })
        .collect()
}

const LONG_REC: Hold = Hold { saving: false, long_rec: true, stuck: false };
const SAVING_AND_LONG_REC: Hold = Hold { saving: true, long_rec: true, stuck: false };

#[test]
fn a_hang_during_a_long_recording_restarts_once_and_is_not_held_by_the_recording() {
    // P1: an NVENC hang mid-recording: no packets reach the recording, so waiting for it
    // to end waits forever. The beat stops at second 0.
    assert_eq!(tick(15, |_| ("recording", 3, false, LONG_REC)), [(10, Recovery::Restart)]);
}

#[test]
fn a_save_in_flight_holds_a_hang_restart_for_15_s_only() {
    assert_eq!(tick(30, |_| ("recording", 3, false, SAVING_AND_LONG_REC)), [(25, Recovery::Restart)]);
    // A save that finishes first lets it go at once.
    let at = |s: u64| ("recording", 3, false, if s < 12 { SAVING_AND_LONG_REC } else { LONG_REC });
    assert_eq!(tick(20, at), [(12, Recovery::Restart)]);
}

#[test]
fn the_90_s_bound_is_not_held_by_a_long_recording() {
    // The engine beats, `recovering` (a real error started it) for 90 s visible: failed,
    // then the usual 15 s grace, with the long recording still "running".
    assert_eq!(tick(120, |s| ("recovering", s * 10, false, LONG_REC)), [(106, Recovery::Restart)]);
}

#[test]
fn an_open_in_progress_gets_20_s_before_it_counts_as_hung() {
    // A TDR open can take about 10 s: the engine says an open is in progress.
    assert_eq!(tick(30, |_| ("recovering", 3, true, Hold::default())), [(20, Recovery::Restart)]);
    assert_eq!(tick(15, |_| ("recovering", 3, false, Hold::default())), [(10, Recovery::Restart)]);
}

#[test]
fn a_second_hang_soon_after_the_restart_gives_up_once_until_the_game_changes() {
    let at = |s: u64| match s {
        0..=10 => ("recording", 3, false, Hold::default()), // hung: restart at 10
        11..=14 => ("starting", 0, false, Hold::default()),
        15..=20 => ("recording", s, false, Hold::default()), // beats a few seconds
        _ => ("recording", 20, false, Hold::default()),     // hangs again: due at 30
    };
    assert_eq!(tick(400, at), [(10, Recovery::Restart), (30, Recovery::GaveUp)]);
}

#[test]
fn giving_up_ends_with_a_new_game_or_a_settings_change() {
    let t0 = Instant::now();
    let up = EngineHealth::Running { pid: 9 };
    let mut r = EncoderRecovery::default();
    let step = |r: &mut EncoderRecovery, s: u64, state, progress, target| {
        r.step_held(t0 + Duration::from_secs(s), state, up, progress, false, true, Hold::default(), target)
    };
    for s in 0..=10 {
        step(&mut r, s, "recording", 3, "game-a");
    }
    for s in 11..=30 {
        step(&mut r, s, "recording", 4, "game-a");
    }
    assert_eq!(step(&mut r, 31, "recording", 4, "game-a"), Recovery::Nothing, "given up: no restart");
    // The settings change: one more chance.
    r.settings_changed();
    let out: Vec<Recovery> = (32..=50).map(|s| step(&mut r, s, "recording", 5, "game-a")).filter(|o| *o != Recovery::Nothing).collect();
    assert_eq!(out, [Recovery::Restart]);
    // A new game after another give-up: also one more chance.
    let out: Vec<Recovery> = (61..=120).map(|s| step(&mut r, s, "recording", 6, "game-a")).filter(|o| *o != Recovery::Nothing).collect();
    assert_eq!(out, [Recovery::GaveUp]);
    let out: Vec<Recovery> = (121..=140).map(|s| step(&mut r, s, "recording", 7, "game-b")).filter(|o| *o != Recovery::Nothing).collect();
    assert_eq!(out, [Recovery::Restart]);
}

#[test]
fn the_restart_after_a_hang_ends_a_running_long_recording_early_first() {
    // The quit path cannot finalize what a hung encoder holds, so the controller asks the
    // engine's recording thread (alive) to end the recording as failed before the
    // restart: every part is kept and the app says "Recording ended early".
    let lines = restart_lines(true);
    assert_eq!(lines.len(), 1);
    let v: serde_json::Value = serde_json::from_str(&lines[0]).unwrap();
    assert_eq!(v["cmd"], "record_fail");
    assert_eq!(v["reason"], "the encoder isn't responding");
    assert!(restart_lines(false).is_empty(), "no recording, nothing to end");
}

#[test]
fn a_hang_during_a_long_recording_end_to_end_ends_it_early_and_restarts_once() {
    // The controller side of hang -> status -> restart, as `encoder_recover_tick` runs it:
    // the encoder thread stops beating at 30 s mid-recording; the restart sends
    // `record_fail` first (the engine's recording thread then finishes every part and
    // reports `recording_failed`: `longrec::tests::record_fail_ends_a_continued_recording_
    // early_and_keeps_every_part`; the app's toast says "Recording ended early"), and the
    // new engine records again with no long recording.
    let t0 = Instant::now();
    let up = EngineHealth::Running { pid: 9 };
    let mut r = EncoderRecovery::default();
    let (mut restarts, mut sent, mut long_rec, mut restarted_at) = (0, Vec::new(), true, None);
    for s in 0..=300u64 {
        let (state, progress) = match restarted_at {
            None if s < 30 => ("recording", s),
            None => ("recording", 30), // hung
            Some(at) if s < at + 4 => ("starting", 0),
            Some(_) => ("recording", s), // the new engine beats
        };
        let hold = Hold { long_rec, ..Hold::default() };
        if r.step_held(t0 + Duration::from_secs(s), state, up, progress, false, true, hold, "game-a") == Recovery::Restart {
            restarts += 1;
            sent.extend(restart_lines(long_rec));
            long_rec = false; // the recording ended early; the new engine has none
            restarted_at = Some(s);
        }
    }
    assert_eq!(restarts, 1, "exactly one restart");
    assert_eq!(restarted_at, Some(40), "10 s after the beat stopped, not after the recording");
    assert_eq!(sent.len(), 1);
    assert!(sent[0].contains("\"cmd\":\"record_fail\""), "{sent:?}");
}

#[test]
fn the_restart_waits_up_to_20_s_for_the_recording_to_end_early() {
    // record_fail went out: the recording thread may need a while to finish the part on a
    // slow HDD. The quit waits for `recording_failed` (the activity leaves recording), at
    // most 20 s; then it quits anyway and the next start recovers the .part.
    let t0 = Instant::now();
    let wait = EndingEarly::new(t0, 0);
    assert!(!wait.restart_now(t0 + Duration::from_secs(1), true), "still finishing the part");
    assert!(!wait.restart_now(t0 + Duration::from_secs(19), true));
    assert!(wait.restart_now(t0 + Duration::from_secs(3), false), "recording_failed came: quit now");
    assert!(wait.restart_now(t0 + Duration::from_secs(20), true), "20 s: quit anyway");
}

#[test]
fn a_healthy_minute_after_giving_up_lets_a_later_failure_restart_again() {
    let t0 = Instant::now();
    let up = EngineHealth::Running { pid: 9 };
    let mut r = EncoderRecovery::default();
    let mut outs = Vec::new();
    for s in 0..=140u64 {
        let (state, progress) = match s {
            0..=10 => ("recording", 3),  // hung: restart at 10
            11..=30 => ("recording", 4), // hung again at 21: gives up
            31..=110 => ("recording", s), // then it beats: a healthy minute by 91
            _ => ("error", s),           // a real `failed` later in the same game
        };
        let out = r.step_held(t0 + Duration::from_secs(s), state, up, progress, false, true, Hold::default(), "game-a");
        if out != Recovery::Nothing && out != Recovery::Recovered {
            outs.push((s, out));
        }
    }
    assert_eq!(outs, [(10, Recovery::Restart), (21, Recovery::GaveUp), (127, Recovery::Restart)]);
}

#[test]
fn a_new_engine_during_the_wait_cancels_the_restart() {
    // The supervisor restarted a crashed engine while record_fail waited: the fresh engine
    // (a new hello) must not be restarted again.
    let t0 = Instant::now();
    let wait = EndingEarly::new(t0, 3);
    assert_eq!(wait.decide(t0 + Duration::from_secs(5), true, 3), EndDecision::Wait);
    assert_eq!(wait.decide(t0 + Duration::from_secs(6), false, 4), EndDecision::Cancel, "a fresh engine restarted again");
    assert_eq!(wait.decide(t0 + Duration::from_secs(6), false, 3), EndDecision::RestartNow);
}
