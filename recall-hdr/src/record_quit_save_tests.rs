//! `save_first` quits: what is saved, and the quit waits for every save it sent.

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
fn a_recording_buffer_and_a_long_recording_are_both_saved() {
    let p = plan(&recording(true));
    assert_eq!(p, SaveFirst { save_buffer: true, stop_long: true, was_recording: true, long_was_on: true });
    assert_eq!(reply_fields(&p), json!({"was_recording": true, "long_was_on": true, "save_first": true}));
}

#[test]
fn a_paused_or_engine_less_recorder_saves_no_buffer() {
    let p = plan(&QuitState { capture_refused: true, ..recording(false) });
    assert_eq!(p, SaveFirst::default(), "nothing captures: nothing to save");
}

#[test]
fn a_privacy_hold_wins_over_save_first() {
    let held = QuitState { privacy_held: Some("Teams.exe is no longer recorded".into()), ..recording(true) };
    let p = plan(&held);
    assert!(!p.save_buffer && !p.stop_long, "a privacy hold saves nothing");
    assert!(p.long_was_on, "the state is still reported");
}

#[test]
fn a_stop_already_in_flight_is_not_sent_again() {
    let p = plan(&QuitState { stop_in_flight: true, ..recording(true) });
    assert!(p.save_buffer && !p.stop_long);
}

#[test]
fn only_the_first_save_first_of_a_quit_sends_saves() {
    let mut q = QuitWait::default();
    assert!(q.take_save_first());
    assert!(!q.take_save_first(), "a repeated save_first quit joins, it does not save twice");
}

/// The engine's answers to a `save_first` quit with a long recording: the quit waits for
/// the buffer's `saved` AND the long recording's `recording_stopped`.
#[test]
fn a_save_first_quit_waits_for_both_saves() {
    let t0 = crate::awake_clock::Awake::ZERO;
    let s = Duration::from_secs;
    let (mut a, mut q) = (EngineActivity::default(), QuitWait::default());
    a.on_event(&json!({"type":"recording_started","path":"L.mp4"}), 0.0);
    assert!(q.take_save_first());
    a.save_sent(t0); // save_clip
    a.stop_sent(); // record_stop
    assert!(q.holds_for_stop(&a));
    assert_eq!(q.step(&mut a, t0), QuitStep::Wait { first: true });
    a.on_engine_event(&json!({"type":"saved","path":"G__buffer.mp4"}), 0.0, t0 + s(2));
    assert_eq!(q.step(&mut a, t0 + s(2)), QuitStep::Wait { first: false }, "the long recording is still being saved");
    a.on_engine_event(&json!({"type":"recording_stopped","path":"L.mp4","seconds":5.0}), 0.0, t0 + s(3));
    assert_eq!(q.step(&mut a, t0 + s(3)), QuitStep::Go, "both saved: the quit goes on");
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
    assert!(!p.save_buffer && !p.stop_long);
    assert_eq!(q.step(&mut a, t0), QuitStep::Go);
}
