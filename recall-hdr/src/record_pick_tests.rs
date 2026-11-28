use std::time::{Duration, Instant};

use serde_json::json;

use super::*;

fn run(start: Option<Instant>, needs_hello: bool, recording: bool) -> PickRun {
    PickRun { label: "Desktop".into(), start_by: start, hello_at_pick: 3, needs_hello, recording, pinned: false, gone: false }
}

#[test]
fn a_pick_names_exactly_one_window_or_monitor() {
    assert_eq!(parse_request(&json!({"cmd":"record_pick","window":4242})), Ok(PickRequest::Window(4242)));
    assert_eq!(parse_request(&json!({"cmd":"record_pick","monitor":r"\\.\DISPLAY2"})), Ok(PickRequest::Monitor(r"\\.\DISPLAY2".into())));
    assert!(parse_request(&json!({"cmd":"record_pick"})).is_err(), "nothing chosen");
    assert!(parse_request(&json!({"cmd":"record_pick","window":0})).is_err(), "no window 0");
    assert!(parse_request(&json!({"cmd":"record_pick","monitor":"  "})).is_err(), "blank monitor");
    assert!(parse_request(&json!({"cmd":"record_pick","window":7,"monitor":r"\\.\DISPLAY1"})).is_err(), "both");
}

#[test]
fn the_start_waits_for_the_engine_on_the_pick() {
    let now = Instant::now();
    let by = now + PICK_START_WAIT;
    // A retarget: the old engine's status does not count; the new engine's hello does.
    assert_eq!(step(&run(Some(by), true, false), now, true, 3, false), PickStep::Wait);
    assert_eq!(step(&run(Some(by), true, false), now, false, 4, false), PickStep::Wait, "hello, not yet applied");
    assert_eq!(step(&run(Some(by), true, false), now, true, 4, false), PickStep::Start);
    // Already on the pick: start at once.
    assert_eq!(step(&run(Some(by), false, false), now, true, 3, false), PickStep::Start);
    // Out of time.
    assert_eq!(step(&run(Some(by), true, false), by + Duration::from_millis(1), false, 3, false), PickStep::GiveUp);
}

#[test]
fn the_pick_holds_until_its_recording_ends() {
    let now = Instant::now();
    assert_eq!(step(&run(None, false, true), now, false, 9, true), PickStep::Wait, "window gone: still recording");
    assert_eq!(step(&run(None, false, true), now, true, 9, false), PickStep::Release);
}

#[test]
fn the_shortcut_asks_only_with_no_game_and_two_monitors() {
    assert!(shortcut_opens_picker(true, 2, false, false));
    assert!(shortcut_opens_picker(true, 3, false, false));
    assert!(!shortcut_opens_picker(true, 1, false, false), "one monitor: nothing to pick");
    assert!(!shortcut_opens_picker(true, 3, false, true), "a game: record it at once");
    assert!(!shortcut_opens_picker(true, 3, true, false), "a recording: the shortcut stops it");
    assert!(!shortcut_opens_picker(false, 3, false, false), "Don't ask again");
}
