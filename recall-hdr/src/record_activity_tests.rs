//! `EngineActivity` / `PendingPause` basics (moved out of `record_activity.rs`).

use super::*;
use crate::awake_clock::Awake;

#[test]
fn a_pause_waits_for_the_long_recording_to_stop_but_not_forever() {
    let t0 = Awake::ZERO;
    let mut a = EngineActivity::default();
    assert_eq!(PendingPause::needed(&mut a, t0), None, "nothing recording: pause at once");
    a.record_requested(t0);
    assert!(PendingPause::needed(&mut a, t0).is_some(), "a starting recording is stopped first too");
    a.on_event(&json!({"type":"recording_started","path":"p"}), 1.0);
    let wait = PendingPause::needed(&mut a, t0).expect("recording: stop first");
    assert!(!wait.ready(&mut a, t0 + Duration::from_secs(1)));
    a.on_event(&json!({"type":"recording_stopped","path":"p","seconds":1.0,"bytes":1}), 0.0);
    assert!(wait.ready(&mut a, t0 + Duration::from_secs(1)), "stopped: pause now");
    a.on_event(&json!({"type":"recording_started","path":"p"}), 1.0);
    assert!(!wait.ready(&mut a, t0 + PAUSE_STOP_WAIT - Duration::from_millis(1)));
    assert!(wait.ready(&mut a, t0 + PAUSE_STOP_WAIT), "an engine that never answers does not block the pause");
}

#[test]
fn saves_in_flight_block_restarts_until_reported_or_stale() {
    let t0 = Awake::ZERO;
    let mut a = EngineActivity::default();
    assert!(!a.busy(t0));
    a.save_sent(t0);
    a.save_sent(t0);
    assert!(a.busy(t0));
    assert_eq!(a.on_event(&json!({"type":"saved","path":"x.mp4"}), 0.0), Some(Trigger::Saved));
    assert!(a.busy(t0), "one more in flight");
    assert_eq!(a.on_event(&json!({"type":"save_failed","reason":"r"}), 0.0), None);
    assert!(!a.busy(t0));
    a.save_sent(t0);
    assert!(!a.busy(t0 + SAVE_STALE), "a lost save does not block forever");
}

#[test]
fn an_engine_exit_with_saves_in_flight_reports_them_lost_once() {
    let t0 = Awake::ZERO;
    let mut a = EngineActivity::default();
    let exited = json!({"type": crate::supervisor::ENGINE_EXITED});
    assert_eq!(a.on_event(&exited, 0.0), None, "nothing in flight: nothing lost");
    a.save_sent(t0);
    a.save_sent(t0);
    assert_eq!(a.on_event(&json!({"type":"saved","path":"x.mp4"}), 0.0), Some(Trigger::Saved));
    assert_eq!(a.on_event(&exited, 0.0), Some(Trigger::SavesLost(1)));
    assert!(!a.busy(t0), "the lost save no longer holds a restart");
    assert_eq!(a.on_event(&exited, 0.0), None, "reported once");
    let line: Value = serde_json::from_str(&lost_save_event()).unwrap();
    assert_eq!(line, json!({"type":"save_failed","reason":"The recorder stopped during the save"}));
}

#[test]
fn long_recording_lifecycle_and_status_shape() {
    let t0 = Awake::ZERO;
    let mut a = EngineActivity::default();
    assert_eq!(a.long_recording_json(), json!({"active":false,"path":null,"started_secs":null}));
    a.record_requested(t0);
    assert!(a.busy(t0) && !a.is_recording());
    a.on_event(&json!({"type":"recording_started","path":"E:\\c\\G__rec.mp4"}), 1_790_000_000.5);
    assert!(a.is_recording());
    let LongRec::Active { since, .. } = a.long.clone() else { panic!("active") };
    assert_eq!(
        a.long_recording_json_at(since),
        json!({"active":true,"path":"E:\\c\\G__rec.mp4","started_secs":1_790_000_000.5,"elapsed_secs":0.0})
    );
    assert_eq!(a.on_event(&json!({"type":"recording_stopped","path":"p","seconds":3.0,"bytes":9}), 0.0), Some(Trigger::LongRecChanged));
    assert!(!a.busy(t0));
    a.record_requested(t0);
    assert!(!a.busy(t0 + START_STALE), "a start that never answered expires");
    a.record_requested(t0);
    a.on_event(&json!({"type":"recording_failed","reason":"x","path":null}), 0.0);
    assert_eq!(a.long, LongRec::Idle);
}

#[test]
fn a_retarget_stops_the_long_recording_once_and_waits_for_it_bounded() {
    let t0 = Awake::ZERO;
    let s = Duration::from_secs;
    let mut a = EngineActivity::default();
    assert!(!a.auto_stop(t0, true), "nothing recording: nothing to stop");
    a.on_event(&json!({"type":"recording_started","path":"p"}), 1.0);
    assert_eq!(a.retarget_busy(t0), Busy { saving: false, long_rec: true });
    assert!(a.auto_stop(t0, true), "first ask sends record_stop");
    assert!(!a.auto_stop(t0 + s(1), true), "sent once per recording");
    assert!(a.retarget_busy(t0 + s(1)).long_rec, "waits for the engine to report it");
    let ev = a.on_event(&json!({"type":"recording_stopped","path":"p","seconds":9.0,"bytes":1}), 0.0);
    assert_eq!(ev, Some(Trigger::AutoStopped { game_closed: true }), "the island says why it stopped");
    assert_eq!(a.retarget_busy(t0 + s(1)), Busy::default(), "stopped: retarget now");
    // An engine that never answers holds the retarget only PAUSE_STOP_WAIT.
    a.on_event(&json!({"type":"recording_started","path":"q"}), 1.0);
    assert!(a.auto_stop(t0 + s(20), false));
    assert!(a.retarget_busy(t0 + s(20) + PAUSE_STOP_WAIT - Duration::from_millis(1)).long_rec);
    assert!(!a.retarget_busy(t0 + s(20) + PAUSE_STOP_WAIT).long_rec, "gave up on the stop");
    // A user's own stop (no auto stop) is the plain event.
    let mut b = EngineActivity::default();
    b.on_event(&json!({"type":"recording_started","path":"p"}), 1.0);
    assert_eq!(b.on_event(&json!({"type":"recording_stopped","path":"p"}), 0.0), Some(Trigger::LongRecChanged));
    // A save in flight still holds, bounded by SAVE_STALE.
    b.save_sent(t0);
    assert_eq!(b.retarget_busy(t0 + s(1)), Busy { saving: true, long_rec: false });
    assert_eq!(b.retarget_busy(t0 + SAVE_STALE), Busy::default());
}

#[test]
fn a_long_recording_cannot_start_while_a_closed_game_drains() {
    let mut a = EngineActivity::default();
    for cmd in ["record_start", "record_toggle"] {
        assert_eq!(a.record_refusal(cmd, true), Some(DRAIN_RECORD_REASON), "{cmd}");
        assert_eq!(a.record_refusal(cmd, false), None, "{cmd}: on target it starts");
    }
    assert_eq!(a.record_refusal("record_stop", true), None);
    a.on_event(&json!({"type":"recording_started","path":"p"}), 1.0);
    assert_eq!(a.record_refusal("record_toggle", true), None, "a toggle that stops is never refused");
    assert_eq!(a.record_refusal("record_start", true), Some(DRAIN_RECORD_REASON));
}

#[test]
fn a_starting_long_recording_never_holds_a_retarget() {
    let t0 = Awake::ZERO;
    let mut a = EngineActivity::default();
    a.record_requested(t0);
    assert_eq!(a.retarget_busy(t0 + Duration::from_secs(1)), Busy::default(), "not yet recording: nothing to save");
    assert!(!a.auto_stop(t0 + Duration::from_secs(1), true), "nothing to stop either");
    a.on_event(&json!({"type":"recording_started","path":"p"}), 1.0);
    assert!(a.retarget_busy(t0 + Duration::from_secs(2)).long_rec, "an active recording is stopped first");
}

#[test]
fn a_game_recreating_its_window_during_a_long_recording_is_never_held_forever() {
    use crate::record_engine::{EnginePlanner, EngineTarget, Step, EXIT_LINGER};
    use crate::record_target::Target;
    let t0 = Awake::ZERO;
    let s = Duration::from_secs;
    let win = |hwnd| EngineTarget::Window(Target { hwnd, pid: 7, name: "G".into(), exe: "G.exe".into() });
    let (mut p, mut a) = (EnginePlanner::new(EXIT_LINGER), EngineActivity::default());
    assert_eq!(p.step(&win(1), (t0).as_test_instant(), a.retarget_busy(t0)), Step::Apply(win(1)));
    a.on_event(&json!({"type":"recording_started","path":"p"}), 1.0);
    // Fullscreen -> windowed: same pid, new hwnd. The engine never answers the stop.
    let mut sent = 0;
    let mut applied_at = None;
    for tick in 1..=3600u64 {
        let now = t0 + s(tick);
        match p.step(&win(2), (now).as_test_instant(), a.retarget_busy(now)) {
            Step::StopLongRec { game_closed } => {
                assert!(!game_closed);
                sent += a.auto_stop(now, game_closed) as u32;
            }
            Step::Apply(t) => {
                assert_eq!(t, win(2));
                applied_at = Some(tick);
                break;
            }
            other => panic!("tick {tick}: {other:?}"),
        }
    }
    assert_eq!(sent, 1, "record_stop sent once");
    assert_eq!(applied_at, Some(1 + PAUSE_STOP_WAIT.as_secs()), "retargeted once the stop timed out");
}

#[test]
fn a_fresh_engine_clears_everything_of_the_old_one() {
    let t0 = Awake::ZERO;
    let mut a = EngineActivity::default();
    a.save_sent(t0);
    a.on_event(&json!({"type":"recording_started","path":"p"}), 1.0);
    a.on_event(&json!({"type": crate::supervisor::ENGINE_EXITED}), 0.0); // read before the hello
    a.engine_restarted();
    assert_eq!(a, EngineActivity::default());
}

/// F2: a save sent to the new engine between its spawn and its hello is not wiped at the
/// hello: a privacy stop then waits for it, and the engine's exit reports it lost.
#[test]
fn a_save_sent_before_the_hello_survives_it_and_holds_the_release() {
    let t0 = Awake::ZERO;
    let s = Duration::from_secs;
    let mut a = EngineActivity::default();
    a.save_sent(t0);
    a.engine_restarted(); // the new engine's hello
    assert_eq!(a.unanswered(), 1, "still counted");
    a.on_engine_event(&json!({"type":"status","save_writing":{"bytes":1,"queued":0,"step":1}}), 0.0, t0 + s(1));
    a.released(t0 + s(2), true);
    assert!(!a.release_may_settle(t0 + s(3)), "settled with the save writing: killed 8 s later");
    let exited = json!({"type": crate::supervisor::ENGINE_EXITED});
    assert_eq!(a.on_engine_event(&exited, 0.0, t0 + s(4)), Some(Trigger::SavesLost(1)));
}

/// F2: a save the engine reports writing that the controller does not count (an older run's
/// count gone) is in flight for the settle and the exit report too, as for the release.
#[test]
fn a_reported_save_without_a_count_holds_the_settle_and_is_reported_at_exit() {
    let t0 = Awake::ZERO;
    let mut a = EngineActivity::default();
    a.on_engine_event(&json!({"type":"save_progress","save_writing":{"bytes":1,"queued":0,"step":1},"stalled_for":0}), 0.0, t0);
    assert!(a.save_in_flight(), "drop_ring releases for it");
    a.released(t0, true);
    assert!(!a.release_may_settle(t0), "and the settle waits for it");
    let exited = json!({"type": crate::supervisor::ENGINE_EXITED});
    assert_eq!(a.on_engine_event(&exited, 0.0, t0), Some(Trigger::SavesLost(1)), "reported, not 0");
}

/// F7: a screenshot whose PNG is still being written holds the settle (the kill would cut it).
#[test]
fn a_pending_screenshot_holds_the_settle() {
    let t0 = Awake::ZERO;
    let mut a = EngineActivity::default();
    a.command_sent("screenshot", t0);
    a.released(t0, false);
    assert!(!a.release_may_settle(t0 + Duration::from_secs(1)), "settled with the PNG in flight");
    a.on_event(&json!({"type":"screenshot","path":"G.png"}), 0.0);
    assert!(a.release_may_settle(t0 + Duration::from_secs(1)));
}

#[test]
fn a_continued_recording_stays_active_between_its_parts() {
    let t0 = Awake::ZERO;
    let mut a = EngineActivity::default();
    a.on_event(&json!({"type":"recording_started","path":"p1","continued":false}), 5.0);
    a.on_event(&json!({"type":"recording_stopped","path":"p1","seconds":9.0,"bytes":1,"continued":true}), 9.0);
    assert!(a.is_recording(), "no gap between the parts for a retarget to slip into");
    assert!(a.retarget_busy(t0).long_rec);
    a.on_event(&json!({"type":"recording_started","path":"p2","continued":true}), 9.5);
    assert!(matches!(&a.long, LongRec::Active { path, started_unix: 5.0, .. } if path == "p2"), "the timer counts from part 1");
    // A retarget's stop sent before the size change is answered by the last part's stop.
    assert!(a.auto_stop(t0, false));
    let ev = a.on_event(&json!({"type":"recording_stopped","path":"p2","seconds":1.0,"bytes":1,"continued":true}), 10.0);
    assert_ne!(ev, Some(Trigger::AutoStopped { game_closed: false }));
    assert!(a.is_recording());
    let ev = a.on_event(&json!({"type":"recording_stopped","path":"p3","seconds":1.0,"bytes":1,"continued":false}), 11.0);
    assert_eq!(ev, Some(Trigger::AutoStopped { game_closed: false }));
    assert_eq!(a.long, LongRec::Idle);
}

#[test]
fn the_elapsed_time_follows_the_monotonic_clock_not_the_wall_clock() {
    let t0 = Instant::now();
    // Started at Unix 1000 s; the wall clock then jumps an hour ahead (NTP, a manual change).
    let long = LongRec::Active { path: "p".into(), started_unix: 1000.0, since: t0 };
    let mut a = EngineActivity { long, ..Default::default() };
    let v = a.long_recording_json_at(t0 + Duration::from_secs(90));
    assert_eq!(v["started_secs"], 1000.0, "the old field stays for older apps");
    assert_eq!(v["elapsed_secs"], 90.0, "90 s recorded, whatever the wall clock says");
    // A continued part keeps the first part's start.
    a.on_event(&json!({"type":"recording_started","path":"p2","continued":true}), 4600.0);
    assert_eq!(a.long_recording_json_at(t0 + Duration::from_secs(95))["elapsed_secs"], 95.0);
    assert_eq!(a.long_recording_json_at(t0)["elapsed_secs"], 0.0);
}
