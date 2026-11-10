//! Saves queued behind each other, and screenshots in flight, as the
//! controller counts them before an engine restart.

use std::time::Duration;

use serde_json::json;

use super::*;
use crate::awake_clock::Awake;
use crate::record_engine::{EnginePlanner, EngineTarget, Step, EXIT_LINGER};
use crate::record_target::Target;

fn win(pid: u32, hwnd: isize) -> EngineTarget {
    EngineTarget::Window(Target { hwnd, pid, name: "G".into(), exe: "G.exe".into() })
}

fn saved() -> serde_json::Value {
    json!({"type":"saved","path":"G__x.mp4","seconds":30.0})
}

#[test]
fn queued_saves_that_keep_answering_hold_a_restart_until_the_last_one() {
    let t0 = Awake::ZERO;
    let s = Duration::from_secs;
    let (mut p, mut a) = (EnginePlanner::new(EXIT_LINGER), EngineActivity::default());
    assert_eq!(p.step(&win(7, 1), (t0).as_test_instant(), a.retarget_busy(t0)), Step::Apply(win(7, 1)));
    for _ in 0..3 {
        a.save_sent(t0);
    }
    // Each save takes 90 s to write: they answer at 90 s and 180 s, the last at 270 s.
    a.on_engine_event(&saved(), 0.0, t0 + s(90));
    a.on_engine_event(&saved(), 0.0, t0 + s(180));
    assert!(a.busy(t0 + s(200)), "a settings restart at 200 s waits for the save still writing");
    assert_eq!(p.step(&win(8, 2), (t0 + s(200)).as_test_instant(), a.retarget_busy(t0 + s(200))), Step::Held, "a game switch waits too");
    assert!(a.busy(t0 + s(269)), "the engine answered 89 s ago: still alive");
    a.on_engine_event(&saved(), 0.0, t0 + s(270));
    assert!(!a.busy(t0 + s(270)));
    assert_eq!(p.step(&win(8, 2), (t0 + s(270)).as_test_instant(), a.retarget_busy(t0 + s(270))), Step::Apply(win(8, 2)));
}

#[test]
fn a_wedged_engine_holds_its_queue_one_window_not_one_per_save() {
    let t0 = Awake::ZERO;
    let s = Duration::from_secs;
    let mut a = EngineActivity::default();
    a.save_sent(t0);
    a.save_sent(t0 + s(50));
    a.save_sent(t0 + s(100));
    assert!(a.busy(t0 + SAVE_STALE - s(1)));
    assert!(!a.busy(t0 + SAVE_STALE), "no answer for 120 s since the queue started: all three are lost");
    // A fresh queue after that starts its own window.
    a.save_sent(t0 + s(200));
    assert!(a.busy(t0 + s(200) + SAVE_STALE - s(1)));
    assert!(!a.busy(t0 + s(200) + SAVE_STALE));
}

#[test]
fn a_failed_save_is_an_answer_too() {
    let t0 = Awake::ZERO;
    let s = Duration::from_secs;
    let mut a = EngineActivity::default();
    a.save_sent(t0);
    a.save_sent(t0);
    a.on_engine_event(&json!({"type":"save_failed","reason":"r"}), 0.0, t0 + s(100));
    assert!(a.busy(t0 + s(219)), "the engine answered at 100 s: the next save gets its window");
    assert!(!a.busy(t0 + s(220)));
}

#[test]
fn a_screenshot_is_refused_while_a_closed_game_drains() {
    let a = EngineActivity::default();
    assert_eq!(a.record_refusal("screenshot", true), Some(DRAIN_SCREENSHOT_REASON), "the closed game's last frame");
    assert_eq!(a.record_refusal("screenshot", false), None, "on target it is taken");
    assert_eq!(a.record_refusal("bookmark", true), None);
}

#[test]
fn a_screenshot_in_flight_holds_a_retarget_until_reported_or_stale() {
    use crate::record_engine::{EnginePlanner, EngineTarget, Step, EXIT_LINGER};
    use crate::record_target::Target;
    let t0 = Awake::ZERO;
    let s = Duration::from_secs;
    let win = |pid, hwnd| EngineTarget::Window(Target { hwnd, pid, name: "G".into(), exe: "G.exe".into() });
    let (mut p, mut a) = (EnginePlanner::new(EXIT_LINGER), EngineActivity::default());
    assert_eq!(p.step(&win(7, 1), (t0).as_test_instant(), a.retarget_busy(t0)), Step::Apply(win(7, 1)));
    a.command_sent("screenshot", t0);
    assert!(a.busy(t0), "a settings restart waits for the PNG");
    assert_eq!(p.step(&win(8, 2), (t0 + s(1)).as_test_instant(), a.retarget_busy(t0 + s(1))), Step::Held, "a new game waits for the PNG");
    a.on_event(&json!({"type":"screenshot","path":"G.png"}), 0.0);
    assert!(!a.busy(t0 + s(1)));
    assert_eq!(p.step(&win(8, 2), (t0 + s(1)).as_test_instant(), a.retarget_busy(t0 + s(1))), Step::Apply(win(8, 2)));
    a.command_sent("screenshot", t0 + s(2));
    a.on_event(&json!({"type":"screenshot_failed","reason":"r"}), 0.0);
    assert!(!a.busy(t0 + s(2)), "a failed screenshot is over too");
    a.command_sent("screenshot", t0 + s(3));
    assert!(a.busy(t0 + s(3) + SCREENSHOT_STALE - Duration::from_millis(1)));
    assert!(!a.busy(t0 + s(3) + SCREENSHOT_STALE), "a lost screenshot does not block forever");
    assert_eq!(a.retarget_busy(t0 + s(3) + SCREENSHOT_STALE), Busy::default());
}

#[test]
fn screenshots_in_flight_are_counted_and_go_stale_after_60_s() {
    let t0 = Awake::ZERO;
    let s = Duration::from_secs;
    let mut a = EngineActivity::default();
    a.command_sent("screenshot", t0);
    a.command_sent("screenshot", t0 + s(1));
    a.on_event(&json!({"type":"screenshot","path":"a.png"}), 0.0);
    assert!(a.busy(t0 + s(1)), "the second PNG is still being written");
    assert!(a.retarget_busy(t0 + s(1)).saving, "and it holds a retarget");
    a.on_event(&json!({"type":"screenshot_failed","reason":"r"}), 0.0);
    assert!(!a.busy(t0 + s(1)), "both reported");
    a.on_event(&json!({"type":"screenshot","path":"x.png"}), 0.0);
    assert!(!a.busy(t0 + s(1)), "a stray report does not underflow");
    assert_eq!(SCREENSHOT_STALE, s(60), "a PNG can go to a sleeping hard drive");
    a.command_sent("screenshot", t0 + s(2));
    assert!(a.busy(t0 + s(2) + s(59)), "a slow disk is not a lost screenshot");
    assert!(!a.busy(t0 + s(2) + SCREENSHOT_STALE), "a lost one does not block forever");
}

// ---- status.save_writing: a long save to a slow drive (R4-overlay-1/2/3) ----

fn writing(bytes: u64) -> serde_json::Value {
    json!({"type":"status","encoder":"h264_nvenc","save_writing":{"bytes":bytes,"queued":0}})
}

fn idle_status() -> serde_json::Value {
    json!({"type":"status","encoder":"h264_nvenc"})
}

#[test]
fn a_save_still_growing_after_120_s_holds_a_settings_restart_until_saved() {
    let t0 = Awake::ZERO;
    let s = Duration::from_secs;
    let (mut p, mut a) = (EnginePlanner::new(EXIT_LINGER), EngineActivity::default());
    assert_eq!(p.step(&win(7, 1), (t0).as_test_instant(), a.retarget_busy(t0)), Step::Apply(win(7, 1)));
    a.save_sent(t0);
    // 11 GB at 42 MB/s: about 265 s, one status line every 5 s here.
    for tick in 0..=53u64 {
        a.on_engine_event(&writing(tick * 210_000_000), 0.0, t0 + s(tick * 5));
        let now = t0 + s(tick * 5 + 1);
        assert!(a.busy(now), "{} s: a settings restart waits while the bytes grow", tick * 5 + 1);
    }
    assert_eq!(p.step(&win(8, 2), (t0 + s(266)).as_test_instant(), a.retarget_busy(t0 + s(266))), Step::Held, "a game switch waits too");
    a.on_engine_event(&saved(), 0.0, t0 + s(267));
    a.on_engine_event(&idle_status(), 0.0, t0 + s(268));
    assert!(!a.busy(t0 + s(268)), "saved and the saver is empty: restart now");
    assert_eq!(p.step(&win(8, 2), (t0 + s(268)).as_test_instant(), a.retarget_busy(t0 + s(268))), Step::Apply(win(8, 2)));
}

#[test]
fn bytes_stuck_for_120_s_still_expire() {
    let t0 = Awake::ZERO;
    let s = Duration::from_secs;
    let mut a = EngineActivity::default();
    a.save_sent(t0);
    a.on_engine_event(&writing(1_000), 0.0, t0 + s(60));
    a.on_engine_event(&writing(2_000), 0.0, t0 + s(110));
    a.on_engine_event(&writing(3_000), 0.0, t0 + s(170));
    assert!(a.busy(t0 + s(200)), "grew 30 s ago");
    for tick in 171..=300u64 {
        a.on_engine_event(&writing(3_000), 0.0, t0 + s(tick)); // a wedged writer still reports
    }
    assert!(a.busy(t0 + s(289)));
    assert!(!a.busy(t0 + s(290)), "no growth for 120 s: a wedged writer does not hold forever");
    assert!(!a.retarget_busy(t0 + s(290)).saving);
}

#[test]
fn an_engine_without_save_writing_keeps_the_plain_window() {
    let t0 = Awake::ZERO;
    let s = Duration::from_secs;
    let mut a = EngineActivity::default();
    a.save_sent(t0);
    for tick in 1..=130u64 {
        a.on_engine_event(&idle_status(), 0.0, t0 + s(tick));
    }
    assert!(!a.busy(t0 + SAVE_STALE), "an old engine: today's 120 s");
}

#[test]
fn a_late_answer_for_an_expired_queue_does_not_end_the_new_one() {
    let t0 = Awake::ZERO;
    let s = Duration::from_secs;
    let mut a = EngineActivity::default();
    a.save_sent(t0);
    a.save_sent(t0);
    assert!(!a.busy(t0 + s(120)), "both expired unanswered");
    a.save_sent(t0 + s(125));
    a.on_engine_event(&saved(), 0.0, t0 + s(130)); // the first old save, late
    assert!(a.busy(t0 + s(131)), "the new save is still in the engine's queue");
    a.on_engine_event(&saved(), 0.0, t0 + s(140)); // the second old save
    assert!(a.busy(t0 + s(141)));
    a.on_engine_event(&saved(), 0.0, t0 + s(150)); // the new one
    assert!(!a.busy(t0 + s(151)));
}

#[test]
fn an_engine_rejection_moves_no_window() {
    let t0 = Awake::ZERO;
    let s = Duration::from_secs;
    let mut a = EngineActivity::default();
    for _ in 0..3 {
        a.save_sent(t0);
    }
    a.save_sent(t0 + s(100));
    a.on_engine_event(&json!({"type":"save_failed","reason":"previous save still writing","rejected":true}), 0.0, t0 + s(100));
    assert!(a.busy(t0 + s(119)));
    assert!(!a.busy(t0 + s(120)), "a rejection shows the main loop alive, not the writer");
}

#[test]
fn a_save_writing_across_a_sleep_keeps_its_window_on_the_awake_clock() {
    let t0 = Awake::ZERO;
    let s = Duration::from_secs;
    let mut a = EngineActivity::default();
    a.save_sent(t0);
    a.on_engine_event(&writing(5_000), 0.0, t0 + s(10));
    // The PC slept at 20 s for an hour: the awake clock stood still (it used to need a
    // `resumed` re-stamp because the clock ran on).
    let woke = t0 + s(20);
    assert!(a.saves_writing(woke), "a resume restart waits for the save being written");
    a.on_engine_event(&writing(6_000), 0.0, woke + s(5));
    assert!(a.saves_writing(woke + s(100)));
    assert!(!a.saves_writing(woke + s(125)), "stuck after the wake: bounded as ever");
}

// ---- round 5: pause, cap, progress steps (R5-overlay-1/4/7/8) ----

fn stepping(step: u64) -> serde_json::Value {
    json!({"type":"status","encoder":"h264_nvenc","save_writing":{"bytes":0,"queued":0,"step":step}})
}

#[test]
fn a_pause_waits_for_the_save_being_written() {
    let t0 = Awake::ZERO;
    let s = Duration::from_secs;
    let mut a = EngineActivity::default();
    a.save_sent(t0);
    a.on_engine_event(&writing(1_000_000_000), 0.0, t0 + s(5));
    let wait = PendingPause::needed(&mut a, t0 + s(6)).expect("a save is being written: the pause waits");
    assert!(!wait.ready(&mut a, t0 + s(7)));
    assert!(!wait.ready(&mut a, t0 + s(6) + PAUSE_STOP_WAIT), "the long recording's 10 s does not end a save's wait");
    a.on_engine_event(&saved(), 0.0, t0 + s(20));
    a.on_engine_event(&idle_status(), 0.0, t0 + s(21));
    assert!(wait.ready(&mut a, t0 + s(21)), "saved: pause now");
    assert_eq!(PendingPause::needed(&mut a, t0 + s(22)), None, "nothing in flight: pause at once");
}

#[test]
fn the_aac_phase_counts_as_progress_through_its_steps() {
    // R5-overlay-4: no `.part` yet (bytes 0), the saver's step counter rises.
    let t0 = Awake::ZERO;
    let s = Duration::from_secs;
    let mut a = EngineActivity::default();
    a.save_sent(t0);
    for (i, at) in [50u64, 100, 150].into_iter().enumerate() {
        a.on_engine_event(&stepping(i as u64 + 1), 0.0, t0 + s(at));
    }
    assert!(a.busy(t0 + s(160)), "the encode moves on: a restart waits");
    assert!(a.busy(t0 + s(269)));
    assert!(!a.busy(t0 + s(270)), "120 s with no step: stale as before");
}

#[test]
fn a_null_save_writing_is_not_a_save() {
    let t0 = Awake::ZERO;
    let mut a = EngineActivity::default();
    a.on_engine_event(&json!({"type":"status","save_writing":null}), 0.0, t0);
    assert!(!a.busy(t0 + Duration::from_secs(1)), "null reads as no save in the saver");
}

#[test]
fn a_growing_save_holds_restarts_at_most_10_min_or_its_size_at_5_mb_s() {
    let t0 = Awake::ZERO;
    let s = Duration::from_secs;
    let mut a = EngineActivity::default();
    a.set_save_estimate(1_000_000_000); // 1 GB: 200 s at 5 MB/s, so the 10 min floor applies
    a.save_sent(t0);
    for sec in 0..=700u64 {
        a.on_engine_event(&writing(sec * 1_000_000), 0.0, t0 + s(sec)); // 1 MB/s: a dying drive
    }
    assert!(a.busy(t0 + s(599)), "still under the cap");
    assert!(!a.busy(t0 + s(700)), "past 10 min the next game wins (today's behaviour after the cap)");
    // An 11 GB clip: 2200 s at 5 MB/s.
    let mut b = EngineActivity::default();
    b.set_save_estimate(11_000_000_000);
    b.save_sent(t0);
    for sec in 0..=2300u64 {
        b.on_engine_event(&writing(sec * 1_000_000), 0.0, t0 + s(sec));
    }
    assert!(b.busy(t0 + s(2199)));
    assert!(!b.busy(t0 + s(2200)));
}

#[test]
fn a_restart_held_30_s_by_a_save_says_so_once() {
    let t0 = Awake::ZERO;
    let s = Duration::from_secs;
    let mut a = EngineActivity::default();
    a.save_sent(t0);
    let mut shown = Vec::new();
    for sec in 0..=200u64 {
        a.on_engine_event(&writing(sec * 1_000_000), 0.0, t0 + s(sec));
        let held = sec >= 10; // a game switch falls due at 10 s
        if a.hold_notice(t0 + s(sec), held) {
            shown.push(sec);
        }
    }
    assert_eq!(shown, [40], "held 30 s: one notice per save");
    assert!(!a.hold_notice(t0 + s(201), false));
}

#[test]
fn a_save_still_growing_is_told_apart_from_one_only_counted() {
    let t0 = Awake::ZERO;
    let s = Duration::from_secs;
    let mut a = EngineActivity::default();
    a.save_sent(t0);
    assert!(!a.save_growing(t0 + s(1)), "counted, no progress report: not growing");
    a.on_engine_event(&stepping(1), 0.0, t0 + s(2));
    assert!(a.save_growing(t0 + s(11)));
    assert!(!a.save_growing(t0 + s(12)), "no step for 10 s");
    assert_eq!(a.saves_in_flight(), 1);
}
