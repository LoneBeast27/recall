//! `save_first` quits: the replay buffer is never saved, a long recording is stopped and
//! saved, and the quit waits only for that recording's `recording_stopped`.

use std::time::Duration;

use serde_json::json;

use super::*;
use crate::record_activity::EngineActivity;
use crate::record_quit_wait::{QuitStep, QuitWait};

fn recording(long_on: bool) -> QuitState {
    QuitState { capture_refused: false, privacy_held: None, long_on, stop_in_flight: false }
}

#[test]
fn save_first_is_read_only_from_a_true_flag() {
    assert!(wants_save_first(&json!({"cmd":"quit","save_first":true})));
    assert!(!wants_save_first(&json!({"cmd":"quit"})));
    assert!(!wants_save_first(&json!({"cmd":"quit","save_first":"yes"})));
}

#[test]
fn a_long_recording_is_stopped_and_the_state_reported_for_the_restore() {
    let p = plan(&recording(true));
    assert_eq!(p, SaveFirst { stop_long: true, was_recording: true, long_was_on: true });
    assert_eq!(reply_fields(&p), json!({"was_recording": true, "long_was_on": true, "save_first": true}));
}

#[test]
fn a_recording_buffer_without_a_long_recording_sends_nothing() {
    let p = plan(&recording(false));
    assert_eq!(p, SaveFirst { stop_long: false, was_recording: true, long_was_on: false }, "the buffer is never saved");
    assert_eq!(reply_fields(&p), json!({"was_recording": true, "long_was_on": false, "save_first": true}));
}

#[test]
fn a_paused_or_engine_less_recorder_sends_nothing() {
    let p = plan(&QuitState { capture_refused: true, ..recording(false) });
    assert_eq!(p, SaveFirst::default());
}

#[test]
fn a_privacy_hold_wins_over_save_first() {
    let held = QuitState { privacy_held: Some("Teams.exe is no longer recorded".into()), ..recording(true) };
    let p = plan(&held);
    assert!(!p.stop_long, "a privacy hold stops nothing");
    assert!(p.was_recording && p.long_was_on, "the state is still reported");
}

#[test]
fn a_stop_already_in_flight_is_not_sent_again() {
    let p = plan(&QuitState { stop_in_flight: true, ..recording(true) });
    assert!(!p.stop_long && p.long_was_on);
}

#[test]
fn only_the_first_save_first_of_a_quit_sends_the_stop() {
    let mut q = QuitWait::default();
    assert!(q.take_save_first());
    assert!(!q.take_save_first(), "a repeated save_first quit joins, it does not stop twice");
}

/// The engine's answers to a `save_first` quit with a long recording: no buffer save is
/// sent, and the quit waits for the long recording's `recording_stopped` alone.
#[test]
fn a_save_first_quit_waits_only_for_the_long_recording() {
    let t0 = crate::awake_clock::Awake::ZERO;
    let s = Duration::from_secs;
    let (mut a, mut q) = (EngineActivity::default(), QuitWait::default());
    a.on_event(&json!({"type":"recording_started","path":"L.mp4"}), 0.0);
    assert!(q.take_save_first());
    a.stop_sent(); // record_stop: the only command a save_first quit sends
    assert_eq!(a.unanswered(), 0, "no save_clip was counted");
    assert!(q.holds_for_stop(&a));
    assert_eq!(q.step(&mut a, t0), QuitStep::Wait { first: true });
    assert_eq!(q.step(&mut a, t0 + s(2)), QuitStep::Wait { first: false }, "the long recording is still being saved");
    a.on_engine_event(&json!({"type":"recording_stopped","path":"L.mp4","seconds":5.0}), 0.0, t0 + s(3));
    assert_eq!(q.step(&mut a, t0 + s(3)), QuitStep::Go, "the recording is saved: the quit goes on");
}

#[test]
fn a_save_first_quit_with_only_the_buffer_goes_at_once() {
    let t0 = crate::awake_clock::Awake::ZERO;
    let (mut a, mut q) = (EngineActivity::default(), QuitWait::default());
    let p = plan(&recording(false));
    assert!(q.take_save_first());
    assert!(!p.stop_long);
    assert_eq!(a.unanswered(), 0);
    assert_eq!(q.step(&mut a, t0), QuitStep::Go, "nothing was sent, nothing to wait for");
}

#[test]
fn the_long_recording_stop_gives_up_after_the_quit_wait_without_a_save_failure() {
    let t0 = crate::awake_clock::Awake::ZERO;
    let (mut a, mut q) = (EngineActivity::default(), QuitWait::default());
    a.on_event(&json!({"type":"recording_started","path":"L.mp4"}), 0.0);
    q.take_save_first();
    a.stop_sent();
    assert_eq!(q.step(&mut a, t0), QuitStep::Wait { first: true });
    let late = t0 + crate::record_quit_wait::QUIT_SAVE_WAIT;
    assert_eq!(q.step(&mut a, late), QuitStep::Lost(0), "60 s: the engine's quit ends it (.part recovered next start)");
}

#[test]
fn a_plain_quit_does_not_wait_for_a_stop() {
    let t0 = crate::awake_clock::Awake::ZERO;
    let (mut a, mut q) = (EngineActivity::default(), QuitWait::default());
    a.on_event(&json!({"type":"recording_started","path":"L.mp4"}), 0.0);
    a.stop_sent();
    assert_eq!(q.step(&mut a, t0), QuitStep::Go, "as before: the engine's quit finalizes the recording");
}

#[test]
fn a_privacy_held_save_first_quit_sends_nothing_and_goes_at_once() {
    let t0 = crate::awake_clock::Awake::ZERO;
    let (mut a, mut q) = (EngineActivity::default(), QuitWait::default());
    let p = plan(&QuitState { privacy_held: Some("x is no longer recorded".into()), ..recording(false) });
    assert!(q.take_save_first());
    assert!(!p.stop_long);
    assert_eq!(q.step(&mut a, t0), QuitStep::Go);
}
