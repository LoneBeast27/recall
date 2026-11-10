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


/// R2-detect-r10-p2-1: a released engine's drain sends `save_progress` (no `status` lines
/// any more); while it moves, the save stays in flight past the 120 s window.
#[test]
fn a_released_engines_drain_progress_keeps_its_save_in_flight_past_120_s() {
    let t0 = Instant::now();
    let mut a = EngineActivity::default();
    a.save_sent(t0);
    for i in 1..=60u64 {
        let ev = json!({"type": "save_progress", "save_writing": {"bytes": i * 1000, "queued": 0, "step": i}});
        a.on_engine_event(&ev, 0.0, t0 + Duration::from_secs(i * 5));
    }
    assert!(a.retarget_busy(t0 + Duration::from_secs(300)).saving, "still writing at 300 s");
}

/// SPEC 26 C: the engine's recorder is active from its start (keyframe wait included), so a
/// `record_toggle` while starting stops it: a stop in flight, and no drain refusal for it.
#[test]
fn a_toggle_during_the_keyframe_wait_is_a_stop() {
    let t0 = Instant::now();
    let mut a = EngineActivity::default();
    a.command_sent("record_start", t0);
    assert!(a.is_starting(), "setup: the keyframe wait");
    assert_eq!(a.record_refusal("record_toggle", true), None, "a stop is never refused for a drain");
    a.command_sent("record_toggle", t0);
    assert!(a.stop_in_flight() && a.is_starting(), "the toggle stops it");
    assert_eq!(a.record_refusal("record_toggle", true), None);
}

/// SPEC 26 B: the session end's own stop is ours until another stop is sent (or it ends).
#[test]
fn only_the_session_ends_own_stop_is_ours() {
    let t0 = Instant::now();
    let mut a = EngineActivity::default();
    a.command_sent("record_start", t0);
    a.stop_sent();
    a.claim_stop();
    assert!(a.stop_is_ours());
    a.command_sent("record_stop", t0); // the user's
    assert!(a.stop_in_flight() && !a.stop_is_ours());
    a.stop_sent();
    a.claim_stop();
    a.on_event(&json!({"type": "recording_stopped", "path": "E:/G__rec.mp4"}), 2.0);
    assert!(!a.stop_is_ours(), "it ended");
    a.claim_stop();
    assert!(!a.stop_is_ours(), "nothing in flight to claim");
}
