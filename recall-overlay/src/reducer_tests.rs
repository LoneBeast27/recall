use super::*;
use serde_json::json;

fn status(encoder: &str) -> Value {
    json!({"mod":"engine","event":{"type":"status","source":"Elden Ring","fps":117,"buffer_secs":59.6,"encoder":encoder,"encoder_error":null}})
}

#[test]
fn normal_status_never_shows_the_overlay() {
    let now = Instant::now();
    let mut reducer = Reducer::new();
    assert!(!reducer.view().visible);
    for second in 0..8 {
        reducer.apply_envelope(&status("h264_nvenc"), now + Duration::from_secs(second));
        assert!(!reducer.view().visible);
    }
}

#[test]
fn saved_notification_hides_on_deadline_despite_status_updates() {
    let now = Instant::now();
    let mut reducer = Reducer::new();
    reducer.apply_envelope(&status("h264_nvenc"), now);
    reducer.apply_engine_event(&json!({"type":"saved","path":"clip.mp4"}), now);
    assert!(reducer.view().visible);
    assert_eq!(reducer.view().primary, "Clip saved");
    reducer.apply_envelope(&status("h264_nvenc"), now + SAVED_FOR - Duration::from_millis(1));
    assert!(reducer.view().visible);
    reducer.tick(now + SAVED_FOR);
    assert!(!reducer.view().visible);
    reducer.apply_envelope(&status("h264_nvenc"), now + Duration::from_secs(8));
    assert!(!reducer.view().visible);
}

#[test]
fn mic_feedback_hides_even_while_mic_remains_muted() {
    let now = Instant::now();
    let mut reducer = Reducer::new();
    reducer.apply_envelope(&status("h264_nvenc"), now);
    reducer.apply_engine_event(&json!({"type":"mic_muted","muted":true}), now);
    assert!(reducer.view().visible);
    assert_eq!(reducer.view().primary, "Microphone muted");
    reducer.tick(now + MIC_FOR);
    assert!(!reducer.view().visible);
    assert!(reducer.mic_muted);
}

#[test]
fn repeated_encoder_failure_status_does_not_keep_overlay_visible() {
    let now = Instant::now();
    let mut reducer = Reducer::new();
    let failed = json!({"type":"status","encoder":"failed","encoder_error":"driver failed"});
    reducer.apply_engine_event(&failed, now);
    assert!(reducer.view().visible);
    reducer.apply_engine_event(&failed, now + FAILURE_FOR);
    assert!(!reducer.view().visible);
    reducer.apply_engine_event(&failed, now + FAILURE_FOR + Duration::from_secs(1));
    assert!(!reducer.view().visible);
}

#[test]
fn a_new_failure_after_expiry_requests_native_visibility_sync() {
    let now = Instant::now();
    let mut reducer = Reducer::new();
    let first = json!({"type":"status","encoder":"failed","encoder_error":"first failure"});
    let second = json!({"type":"status","encoder":"failed","encoder_error":"second failure"});
    reducer.apply_engine_event(&first, now);
    reducer.apply_engine_event(&first, now + FAILURE_FOR);
    assert!(!reducer.view().visible);
    assert!(reducer.apply_engine_event(&second, now + FAILURE_FOR + Duration::from_secs(1)));
    assert!(reducer.view().visible);
}

#[test]
fn stale_feed_does_not_cut_short_saved_notification() {
    let now = Instant::now();
    let mut reducer = Reducer::new();
    reducer.apply_envelope(&status("h264_nvenc"), now);
    reducer.apply_engine_event(&json!({"type":"saved","path":"clip.mp4"}), now);
    reducer.tick(now + STALE_AFTER + Duration::from_millis(1));
    assert!(reducer.view().visible);
    reducer.tick(now + SAVED_FOR);
    assert!(!reducer.view().visible);
}

#[test]
fn ready_status_is_truthful_and_labels_capture_fps() {
    let now = Instant::now();
    let mut reducer = Reducer::new();
    assert!(reducer.apply_envelope(&status("h264_nvenc"), now));
    assert_eq!(reducer.view().state, RecorderState::Ready);
    assert_eq!(reducer.view().primary, "Replay ready");
    assert!(reducer.view().metrics.contains("Capture 117 FPS"));
    assert!(reducer.view().metrics.contains("60s buffer"));
    assert_eq!(reducer.view().secondary, "Source: Elden Ring");
}

#[test]
fn stale_status_can_never_remain_ready() {
    let now = Instant::now();
    let mut reducer = Reducer::new();
    reducer.apply_envelope(&status("h264_nvenc"), now);
    assert!(reducer.tick(now + STALE_AFTER + Duration::from_millis(1)));
    assert_eq!(reducer.view().state, RecorderState::Stopped);
    assert_eq!(reducer.view().metrics, "Status feed stale");
}

#[test]
fn saved_feedback_expires_back_to_source() {
    let now = Instant::now();
    let mut reducer = Reducer::new();
    reducer.apply_envelope(&status("h264_nvenc"), now);
    reducer.apply_envelope(
        &json!({"mod":"engine","event":{"type":"saved","path":"V:\\clips\\clip.mp4"}}),
        now,
    );
    assert_eq!(reducer.view().secondary, "Saved clip.mp4");
    reducer.apply_envelope(&status("h264_nvenc"), now + Duration::from_secs(2));
    assert!(reducer.tick(now + SAVED_FOR));
    assert_eq!(reducer.view().secondary, "Source: Elden Ring");
}

#[test]
fn failed_encoder_surfaces_reason() {
    let now = Instant::now();
    let mut reducer = Reducer::new();
    let value = json!({"mod":"engine","event":{"type":"status","source":"","fps":0,"buffer_secs":0,"encoder":"failed","encoder_error":"NVENC initialization failed"}});
    reducer.apply_envelope(&value, now);
    assert_eq!(reducer.view().state, RecorderState::Failed);
    assert_eq!(reducer.view().primary, "Encoder failed");
    assert_eq!(reducer.view().secondary, "NVENC initialization failed");
}

#[test]
fn save_failure_surfaces_short_feedback() {
    let now = Instant::now();
    let mut reducer = Reducer::new();
    reducer.apply_envelope(&status("h264_nvenc"), now);
    reducer.apply_envelope(
        &json!({"mod":"engine","event":{"type":"save_failed","reason":"the output folder is unavailable"}}),
        now,
    );
    assert_eq!(
        reducer.view().secondary,
        "Save failed: the output folder is unavailable"
    );
}

fn chord(active: bool) -> Value {
    if !active {
        return json!({"mod":"session","event":{"type":"chord","active":false}});
    }
    json!({"mod":"session","event":{"type":"chord","active":true,"leader":"Alt `","options":[
        {"key":"1","label":"Bookmark"},{"key":"2","label":"Save 30 s"},
        {"key":"3","label":"Screenshot"},{"key":"4","label":"Record"}]}})
}

#[test]
fn chord_hint_card_shows_the_four_options_and_closes_on_inactive() {
    let now = Instant::now();
    let mut reducer = Reducer::new();
    reducer.apply_envelope(&status("h264_nvenc"), now);
    assert!(reducer.apply_envelope(&chord(true), now));
    let v = reducer.view();
    assert!(v.visible);
    assert_eq!((v.primary.as_str(), v.metrics.as_str(), v.secondary.as_str()), ("Recall", "1 Bookmark · 2 Save 30 s", "3 Screenshot · 4 Record"));
    // A status tick while the window is open keeps the hint.
    reducer.apply_envelope(&status("h264_nvenc"), now + Duration::from_millis(500));
    assert_eq!(reducer.view().metrics, "1 Bookmark · 2 Save 30 s");
    assert!(reducer.apply_envelope(&chord(false), now + Duration::from_millis(600)));
    assert!(!reducer.view().visible);
    assert!(reducer.view().metrics.contains("60s buffer"), "resting metrics restored");
    assert!(!reducer.apply_envelope(&chord(false), now + Duration::from_millis(700)), "nothing to close twice");
}

#[test]
fn chord_hint_expires_by_itself_and_an_action_toast_replaces_it() {
    let now = Instant::now();
    let mut reducer = Reducer::new();
    reducer.apply_envelope(&status("h264_nvenc"), now);
    reducer.apply_envelope(&chord(true), now);
    assert!(reducer.tick(now + CHORD_FOR));
    assert!(!reducer.view().visible);
    reducer.apply_envelope(&chord(true), now + Duration::from_secs(2));
    reducer.apply_envelope(&json!({"mod":"engine","event":{"type":"bookmarked","at_secs":12.5}}), now + Duration::from_secs(2));
    assert_eq!(reducer.view().primary, "Bookmark added");
    assert!(reducer.view().metrics.contains("buffer"), "the toast uses the normal metrics line");
    assert!(!reducer.apply_envelope(&chord(false), now + Duration::from_secs(2)), "close does not hide the toast");
    assert!(reducer.view().visible);
}

#[test]
fn screenshot_and_long_recording_toasts() {
    let now = Instant::now();
    let mut reducer = Reducer::new();
    reducer.apply_envelope(&status("h264_nvenc"), now);
    reducer.apply_engine_event(&json!({"type":"screenshot","path":"V:\\c\\screenshots\\G__2026-10-22__14-05-09.png"}), now);
    assert_eq!((reducer.view().primary.as_str(), reducer.view().secondary.as_str()), ("Screenshot saved", "Saved G__2026-10-22__14-05-09.png"));
    reducer.apply_engine_event(&json!({"type":"recording_started","path":"V:\\c\\G__rec.mp4"}), now);
    assert_eq!(reducer.view().primary, "Recording started");
    reducer.apply_engine_event(&json!({"type":"recording_stopped","path":"V:\\c\\G__rec.mp4","seconds":754.2,"bytes":1}), now);
    assert_eq!(reducer.view().secondary, "G__rec.mp4 · 12:34");
    reducer.apply_engine_event(&json!({"type":"recording_failed","reason":"disk full","path":null}), now);
    assert_eq!((reducer.view().primary.as_str(), reducer.view().secondary.as_str()), ("Recording failed", "disk full"));
    reducer.apply_engine_event(&json!({"type":"screenshot_failed","reason":"Recording is paused"}), now);
    assert_eq!(reducer.view().primary, "Screenshot failed");
}

#[test]
fn disconnect_clears_ready_state_immediately() {
    let now = Instant::now();
    let mut reducer = Reducer::new();
    reducer.apply_envelope(&status("h264_nvenc"), now);
    assert!(reducer.disconnected());
    assert_eq!(reducer.view().state, RecorderState::Stopped);
    assert!(!reducer.view().connected);
}
