//! The quit wait's pump sleep, a 2nd Quit, and the pipe's `finishing_clip` (R6-overlay-1/3/5,
//! R7-overlay-1/4).

use std::time::{Duration, Instant};

use serde_json::{json, Value};

use super::*;
use crate::record_activity::EngineActivity;
use crate::record_quit::QuitReason;

#[test]
fn the_quit_wait_sleeps_even_when_a_deadline_has_passed() {
    // R6-overlay-1: while quitting `tick` does not run, so a passed janitor / linger /
    // overlay deadline stays passed; the pump must still sleep, not spin.
    let now = Instant::now();
    let past = Some(now - Duration::from_secs(1));
    assert_eq!(pump_wait_ms(true, past, now, 50), QUIT_PUMP_WAIT_MS, "quitting: a 250 ms sleep");
    const { assert!(QUIT_PUMP_WAIT_MS >= 250) };
    assert_eq!(pump_wait_ms(true, None, now, 50), QUIT_PUMP_WAIT_MS);
    assert_eq!(pump_wait_ms(false, past, now, 50), 0, "not quitting: a passed deadline is served at once");
    assert_eq!(pump_wait_ms(false, Some(now + Duration::from_millis(20)), now, 50), 20);
    assert_eq!(pump_wait_ms(false, None, now, 50), 50);
}

#[test]
fn a_second_quit_during_the_save_wait_quits_at_once_and_reports_the_loss() {
    // R6-overlay-5: a long save holds the quit; a 2nd tray Quit stops waiting.
    let t0 = Instant::now();
    let (mut a, mut q) = (EngineActivity::default(), QuitWait::default());
    a.save_sent(t0);
    let reason = on_quit_request(None, QuitReason::TrayMenu, false, t0, &mut q);
    assert_eq!(reason, Some(QuitReason::TrayMenu));
    assert_eq!(q.step(&mut a, t0), QuitStep::Wait { first: true });
    let reason = on_quit_request(reason, QuitReason::TrayMenu, false, t0 + Duration::from_secs(2), &mut q);
    assert_eq!(reason, Some(QuitReason::TrayMenu));
    assert_eq!(q.step(&mut a, t0 + Duration::from_secs(2)), QuitStep::Lost(1), "quit now, the save reported lost");
    let line: Value = serde_json::from_str(&closed_during_save_event()).unwrap();
    assert_eq!(line["reason"], "Recall was closed during the save");
}

#[test]
fn which_repeat_counts_as_quit_now() {
    let t0 = Instant::now();
    let a_wait = || {
        let (mut a, mut q) = (EngineActivity::default(), QuitWait::default());
        a.save_sent(t0);
        q.step(&mut a, t0);
        q
    };
    // A quit before the wait started is just the first quit.
    let later = t0 + Duration::from_secs(2);
    let mut fresh = QuitWait::default();
    assert_eq!(on_quit_request(Some(QuitReason::ControlPipe), QuitReason::ControlPipe, false, later, &mut fresh), Some(QuitReason::ControlPipe));
    assert!(!fresh.forced);
    // The pipe again after a pipe quit (an installer re-run, a double click in the app): join the wait.
    let mut q = a_wait();
    on_quit_request(Some(QuitReason::ControlPipe), QuitReason::ControlPipe, false, later, &mut q);
    assert!(!q.forced, "a plain repeat never throws the clip away");
    // The installer's pipe quit while a tray quit waits: keep waiting, keep the tray's reason.
    let mut q = a_wait();
    assert_eq!(on_quit_request(Some(QuitReason::TrayMenu), QuitReason::ControlPipe, false, later, &mut q), Some(QuitReason::TrayMenu));
    assert!(!q.forced, "an install never throws the user's clip away");
    // A tray Quit while a session end or a pipe quit waits: the user wants out now.
    let mut q = a_wait();
    assert_eq!(on_quit_request(Some(QuitReason::SessionEnd), QuitReason::TrayMenu, false, later, &mut q), Some(QuitReason::TrayMenu));
    assert!(q.forced);
    q.reset();
    assert!(!q.forced && !q.waiting(), "a cancelled quit waits afresh next time");
}

#[test]
fn the_pipe_says_whether_a_quit_is_finishing_a_clip() {
    // R6-overlay-3: the installer waits past 8 s only while this is true.
    assert_eq!(
        finishing_fields(true, 1, Some(42), Some(9_000_000)),
        json!({"finishing_clip": true, "finishing_progress": {"saves": 1, "step": 42, "bytes": 9_000_000}})
    );
    assert_eq!(
        finishing_fields(true, 2, None, None),
        json!({"finishing_clip": true, "finishing_progress": {"saves": 2, "step": null, "bytes": null}})
    );
    assert_eq!(finishing_fields(false, 1, Some(42), Some(1)), json!({"finishing_clip": false, "finishing_progress": null}));
    let quitting: Value = serde_json::from_str(&quitting_event(finishing_fields(true, 1, Some(7), Some(5)))).unwrap();
    assert_eq!(quitting["type"], "quitting");
    assert_eq!(quitting["finishing_clip"], true);
    assert_eq!(quitting["finishing_progress"]["step"], 7);
    assert_eq!(quitting["finishing_progress"]["bytes"], 5);
    let mut status = json!({"type": "status", "state": "recording"});
    merge_fields(&mut status, finishing_fields(true, 1, Some(7), None));
    assert_eq!(status["state"], "recording");
    assert_eq!(status["finishing_clip"], true);
}

#[test]
fn the_finishing_bytes_come_from_the_engines_save_report() {
    let t0 = Instant::now();
    let mut a = EngineActivity::default();
    a.save_sent(t0);
    a.on_engine_event(&json!({"type":"status","save_writing":{"bytes":4_000_000,"queued":0,"step":12}}), 0.0, t0);
    assert_eq!((a.writing_mark(), a.writing_bytes()), (Some(12), Some(4_000_000)));
    a.on_engine_event(&json!({"type":"status","save_writing":null}), 0.0, t0 + Duration::from_secs(1));
    assert_eq!(a.writing_bytes(), None, "no save in the saver: no bytes");
}

#[test]
fn a_repeated_plain_pipe_quit_joins_the_wait_and_only_force_stops_now() {
    // R7-overlay-1: "Stop recorder" clicked twice must not throw the clip away.
    let t0 = Instant::now();
    let (mut a, mut q) = (EngineActivity::default(), QuitWait::default());
    a.save_sent(t0);
    let r = on_quit_request(None, QuitReason::ControlPipe, false, t0, &mut q);
    assert_eq!(q.step(&mut a, t0), QuitStep::Wait { first: true });
    let r = on_quit_request(r, QuitReason::ControlPipe, false, t0 + Duration::from_millis(300), &mut q);
    assert!(!q.forced, "a double click joins the wait");
    assert_eq!(q.step(&mut a, t0 + Duration::from_secs(1)), QuitStep::Wait { first: false });
    on_quit_request(r, QuitReason::ControlPipe, true, t0 + Duration::from_secs(5), &mut q);
    assert!(q.forced, "\"force\":true stops now");
    assert_eq!(q.step(&mut a, t0 + Duration::from_secs(5)), QuitStep::Lost(1));
    // Force on the first quit, or while a tray quit waits, stops now too.
    let mut q = a_wait_at(t0);
    on_quit_request(Some(QuitReason::TrayMenu), QuitReason::ControlPipe, true, t0, &mut q);
    assert!(q.forced);
    let line: Value = serde_json::from_str("{\"cmd\":\"quit\",\"force\":true}").unwrap();
    assert!(quit_force(&line));
    assert!(!quit_force(&json!({"cmd": "quit"})) && !quit_force(&json!({"cmd": "quit", "force": "yes"})));
}

fn a_wait_at(t0: Instant) -> QuitWait {
    let (mut a, mut q) = (EngineActivity::default(), QuitWait::default());
    a.save_sent(t0);
    q.step(&mut a, t0);
    q
}

#[test]
fn a_second_tray_quit_forces_only_a_second_after_the_first() {
    let t0 = Instant::now();
    let (mut a, mut q) = (EngineActivity::default(), QuitWait::default());
    a.save_sent(t0);
    let r = on_quit_request(None, QuitReason::TrayMenu, false, t0, &mut q);
    q.step(&mut a, t0);
    let r = on_quit_request(r, QuitReason::TrayMenu, false, t0 + Duration::from_millis(400), &mut q);
    assert!(!q.forced, "a double click on Quit just joins");
    on_quit_request(r, QuitReason::TrayMenu, false, t0 + TRAY_FORCE_GAP, &mut q);
    assert!(q.forced, "a deliberate 2nd Quit stops now");
}

#[test]
fn a_quit_asked_during_a_sign_out_wait_survives_a_cancelled_sign_out() {
    // R7-overlay-4: the installer's quit must not vanish with the cancelled sign-out.
    let t0 = Instant::now();
    let mut q = a_wait_at(t0);
    let r = on_quit_request(Some(QuitReason::SessionEnd), QuitReason::ControlPipe, false, t0 + Duration::from_secs(1), &mut q);
    assert_eq!(r, Some(QuitReason::SessionEnd));
    assert_eq!(resume_after_session_end(&mut q), Some(QuitReason::ControlPipe), "the pipe quit goes on");
    assert!(!q.waiting() && !q.forced, "and waits afresh");
    // A sign-out alone, cancelled: nothing goes on.
    let mut q = a_wait_at(t0);
    assert_eq!(resume_after_session_end(&mut q), None);
}

#[test]
fn a_held_sign_out_is_reported_until_a_plain_quit_joins_it() {
    // Item 10: an installer sends its own quit only while the quit is the sign-out's alone.
    let t0 = Instant::now();
    let mut q = a_wait_at(t0);
    assert!(sign_out_only(Some(QuitReason::SessionEnd), &q), "quit_for_sign_out: send a plain quit");
    let r = on_quit_request(Some(QuitReason::SessionEnd), QuitReason::ControlPipe, false, t0 + Duration::from_secs(1), &mut q);
    assert!(!q.forced, "a plain quit never forces the save");
    assert!(!sign_out_only(r, &q), "joined: no further quit needed");
    assert!(!sign_out_only(Some(QuitReason::ControlPipe), &QuitWait::default()), "an installer's quit is pending, not a sign-out's");
    assert!(!sign_out_only(None, &QuitWait::default()));
}
