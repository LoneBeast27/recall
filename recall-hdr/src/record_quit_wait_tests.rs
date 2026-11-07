//! The quit wait's pump sleep, a 2nd Quit, and the pipe's `finishing_clip` (R6-overlay-1/3/5).

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
    let reason = on_quit_request(None, QuitReason::TrayMenu, &mut q);
    assert_eq!(reason, Some(QuitReason::TrayMenu));
    assert_eq!(q.step(&mut a, t0), QuitStep::Wait { first: true });
    let reason = on_quit_request(reason, QuitReason::TrayMenu, &mut q);
    assert_eq!(reason, Some(QuitReason::TrayMenu));
    assert_eq!(q.step(&mut a, t0 + Duration::from_secs(1)), QuitStep::Lost(1), "quit now, the save reported lost");
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
    let mut fresh = QuitWait::default();
    assert_eq!(on_quit_request(Some(QuitReason::ControlPipe), QuitReason::ControlPipe, &mut fresh), Some(QuitReason::ControlPipe));
    assert!(!fresh.forced);
    // The pipe again after a pipe quit (a script asking twice): quit now.
    let mut q = a_wait();
    on_quit_request(Some(QuitReason::ControlPipe), QuitReason::ControlPipe, &mut q);
    assert!(q.forced);
    // The installer's pipe quit while a tray quit waits: keep waiting, keep the tray's reason.
    let mut q = a_wait();
    assert_eq!(on_quit_request(Some(QuitReason::TrayMenu), QuitReason::ControlPipe, &mut q), Some(QuitReason::TrayMenu));
    assert!(!q.forced, "an install never throws the user's clip away");
    // A tray Quit while a session end or a pipe quit waits: the user wants out now.
    let mut q = a_wait();
    assert_eq!(on_quit_request(Some(QuitReason::SessionEnd), QuitReason::TrayMenu, &mut q), Some(QuitReason::TrayMenu));
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
