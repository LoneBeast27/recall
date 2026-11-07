//! The drain countdown and the long-recording time count from when the status
//! arrived (monotonic), so a wall-clock jump does not move them.
use std::time::{Duration, Instant};

use serde_json::json;

use super::protocol::State;

fn status(drain: serde_json::Value, long: serde_json::Value) -> State {
    let mut state = State::default();
    state.set_connected(true);
    let line = json!({"mod":"session","event":{"type":"status","state":"draining","engine_health":"running",
        "drain":drain,"long_recording":long}});
    state.apply_line(&line.to_string());
    state
}

#[test]
fn the_drain_countdown_ignores_a_wall_clock_jump() {
    let ends = 1_790_000_120_000u64;
    let state = status(json!({"name":"sobosuba","total_secs":120,"ends_unix_ms":ends,"remaining_secs":110.0}), json!(null));
    let session = state.snapshot.session.clone().expect("session");
    let at = session.received.0.expect("stamped on arrival");
    // 5 s after the status, with the wall clock an hour ahead (it would read 0 left).
    assert_eq!(session.drain_left(ends + 3_600_000, at + Duration::from_secs(5)), Some(105));
    assert_eq!(session.drain_left(ends - 3_600_000, at + Duration::from_millis(500)), Some(110), "rounded up, never past total");
    assert_eq!(session.drain_left(ends, at + Duration::from_secs(200)), Some(0));
}

#[test]
fn an_older_controller_without_remaining_secs_uses_the_end_time() {
    let state = status(json!({"name":"g","total_secs":120,"ends_unix_ms":1_000_000}), json!(null));
    let session = state.snapshot.session.unwrap();
    assert_eq!(session.drain_left(1_000_000 - 30_000, Instant::now()), Some(30));
}

#[test]
fn the_long_recording_time_ignores_a_wall_clock_jump() {
    let state = status(json!(null), json!({"active":true,"path":"E:/rec.mp4","started_secs":1000.0,"elapsed_secs":90.0}));
    let session = state.snapshot.session.clone().expect("session");
    let at = session.received.0.expect("stamped on arrival");
    let since = session.since_status(at + Duration::from_secs(10));
    // The wall clock says 1000 + 3700 s; the recording is 100 s long.
    assert_eq!(session.long_recording.elapsed_at(4700.0, since), 100.0);
    let old = status(json!(null), json!({"active":true,"path":"E:/rec.mp4","started_secs":1000.0}));
    assert_eq!(old.snapshot.session.unwrap().long_recording.elapsed_at(1090.0, since), 90.0, "older controller: wall clock");
}

#[test]
fn a_mic_update_keeps_the_status_arrival_time() {
    let mut state = status(json!({"name":"g","total_secs":120,"ends_unix_ms":1,"remaining_secs":60.0}), json!(null));
    let at = state.snapshot.session.as_ref().unwrap().received.0;
    std::thread::sleep(Duration::from_millis(5));
    state.apply_line(&json!({"mod":"engine","event":{"type":"mic_muted","muted":true}}).to_string());
    assert_eq!(state.snapshot.session.as_ref().unwrap().received.0, at, "only a session status restarts the count");
}

#[test]
fn the_same_status_arriving_again_is_not_a_change() {
    // The app asks for a status every 2 s; an unchanged reply must not repaint.
    let line = json!({"mod":"session","event":{"type":"status","state":"recording","engine_health":"running"}}).to_string();
    let mut state = State::default();
    state.set_connected(true);
    assert!(state.apply_line(&line));
    std::thread::sleep(Duration::from_millis(5));
    assert!(!state.apply_line(&line), "only the arrival time moved");
    assert!(state.snapshot.session.as_ref().unwrap().received.0.is_some(), "the newer arrival is still kept");
}
