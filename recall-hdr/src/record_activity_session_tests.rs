//! EngineActivity as a session end sees it (gate 20b): a start in flight, a stop in flight.

use super::*;

/// Gate 20b P2-b: a start sent just before a session end counts as recording there.
#[test]
fn a_starting_long_recording_counts_for_a_session_end() {
    let t0 = Instant::now();
    let mut a = EngineActivity::default();
    assert!(!a.recording_or_starting());
    a.command_sent("record_start", t0);
    assert!(a.recording_or_starting() && !a.is_recording());
    a.on_event(&json!({"type": "recording_started", "path": "E:/G__rec.mp4"}), 1.0);
    assert!(a.recording_or_starting());
    a.on_event(&json!({"type": "recording_stopped", "path": "E:/G__rec.mp4"}), 2.0);
    assert!(!a.recording_or_starting());
}

/// Gate 20b v3 P1: a stop from any sender is in flight until `recording_stopped` (a
/// continued part's stop is not the end); a stop with nothing recording is not.
#[test]
fn a_stop_is_in_flight_until_the_recording_reports_stopping() {
    let t0 = Instant::now();
    let mut a = EngineActivity::default();
    a.stop_sent();
    assert!(!a.stop_in_flight(), "nothing to stop");
    a.command_sent("record_start", t0);
    a.on_event(&json!({"type": "recording_started", "path": "E:/G__rec.mp4"}), 1.0);
    a.command_sent("record_toggle", t0);
    assert!(a.stop_in_flight() && a.is_recording(), "the toggle stops it; still Active until the engine says so");
    a.on_event(&json!({"type": "recording_stopped", "path": "E:/G__rec.mp4", "continued": true}), 2.0);
    assert!(a.stop_in_flight(), "only a part ended");
    a.on_event(&json!({"type": "recording_stopped", "path": "E:/G__rec.mp4"}), 3.0);
    assert!(!a.stop_in_flight());
    a.command_sent("record_start", t0);
    a.stop_sent(); // a pause / retarget / sleep stop while starting
    assert!(a.stop_in_flight() && a.is_starting());
    a.on_event(&json!({"type": "recording_failed", "reason": "x"}), 4.0);
    assert!(!a.stop_in_flight());
}

