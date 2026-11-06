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
fn a_recovering_engine_is_not_a_failure_and_cpu_copies_are_ready() {
    let now = Instant::now();
    let mut reducer = Reducer::new();
    let recovering = json!({"mod":"engine","event":{"type":"status","source":"","fps":60,"buffer_secs":30.0,"encoder":"recovering","encoder_error":"NVENC encode failed: x"}});
    reducer.apply_envelope(&recovering, now);
    assert_eq!(reducer.view().state, RecorderState::Starting, "the engine rebuilds its encoder; the buffer is kept");
    assert!(!reducer.view().visible, "no 'Encoder failed' pop-up for a recovery");
    reducer.apply_envelope(&status("degraded"), now + Duration::from_secs(1));
    assert_eq!(reducer.view().state, RecorderState::Ready);
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
        &json!({"mod":"engine","event":{"type":"saved","path":"E:\\clips\\Rocket-League__2026-06-06__11-17-50.mp4","seconds":118.8}}),
        now,
    );
    assert_eq!(reducer.view().secondary, "Rocket League · 1:59");
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
    reducer.apply_engine_event(&json!({"type":"screenshot","path":"E:\\c\\screenshots\\G__2026-10-22__14-05-09.png"}), now);
    assert_eq!((reducer.view().primary.as_str(), reducer.view().secondary.as_str()), ("Screenshot saved", "Saved G__2026-10-22__14-05-09.png"));
    reducer.apply_engine_event(&json!({"type":"recording_started","path":"E:\\c\\G__rec.mp4"}), now);
    assert_eq!(reducer.view().primary, "Recording started");
    reducer.apply_engine_event(&json!({"type":"recording_stopped","path":"E:\\c\\G__rec.mp4","seconds":754.2,"bytes":1}), now);
    assert_eq!(reducer.view().secondary, "G__rec.mp4 · 12:34");
    reducer.apply_engine_event(&json!({"type":"recording_failed","reason":"disk full","path":null}), now);
    assert_eq!((reducer.view().primary.as_str(), reducer.view().secondary.as_str()), ("Recording failed", "disk full"));
    reducer.apply_engine_event(&json!({"type":"screenshot_failed","reason":"Recording is paused"}), now);
    assert_eq!(reducer.view().primary, "Screenshot failed");
}

fn encoder_notice() -> Value {
    json!({"mod":"session","event":{"type":"notice","kind":"encoder","title":"Encoder problem","detail":"Recorder stopped"}})
}

#[test]
fn engine_failure_then_controller_encoder_notice_shows_once() {
    let now = Instant::now();
    let mut reducer = Reducer::new();
    let failed = json!({"type":"status","encoder":"failed","encoder_error":"NVENC initialization failed"});
    reducer.apply_engine_event(&failed, now);
    assert_eq!(reducer.view().primary, "Encoder failed");
    assert!(!reducer.apply_envelope(&encoder_notice(), now + Duration::from_secs(1)), "duplicate dropped");
    assert_eq!(reducer.view().primary, "Encoder failed");
    // A later failure (past the window) is news again.
    reducer.tick(now + FAILURE_FOR);
    assert!(reducer.apply_envelope(&encoder_notice(), now + ENCODER_DEDUP + Duration::from_secs(1)));
    assert_eq!(reducer.view().primary, "Encoder problem");
}

#[test]
fn controller_encoder_notice_then_engine_failure_shows_once() {
    let now = Instant::now();
    let mut reducer = Reducer::new();
    reducer.apply_envelope(&status("h264_nvenc"), now);
    assert!(reducer.apply_envelope(&encoder_notice(), now));
    let failed = json!({"type":"status","encoder":"failed","encoder_error":"NVENC initialization failed"});
    reducer.apply_engine_event(&failed, now + Duration::from_millis(300));
    let notice = reducer.view().notice.clone().expect("still the first notice");
    assert_eq!((notice.title.as_str(), notice.detail.as_str()), ("Encoder problem", "Recorder stopped"));
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

/// Merge gate 11-07 (P1/P2): a continued recording showed two notices, always blamed the
/// game's size, and the final notice gave only the last part's length.
#[test]
fn a_continued_long_recording_shows_one_notice_with_its_cause_then_the_whole_length() {
    let now = Instant::now();
    let mut reducer = Reducer::new();
    reducer.apply_envelope(&status("h264_nvenc"), now);
    reducer.apply_engine_event(&json!({"type":"recording_started","path":"E:/c/G__rec.mp4","continued":false,"part":1,"cause":null}), now);
    assert_eq!(reducer.view().primary, "Recording started");
    let stop = json!({"type":"recording_stopped","path":"E:/c/G__rec.mp4","seconds":3540.0,"bytes":1,"continued":true,"parts":1,"total_seconds":3540.0,"cause":"size"});
    reducer.apply_engine_event(&stop, now);
    assert_eq!(reducer.view().primary, "Recording started", "a part's stop shows nothing (no \"saved\")");
    let view = |r: &Reducer| (r.view().primary.clone(), r.view().secondary.clone());
    reducer.apply_engine_event(&json!({"type":"recording_started","path":"E:/c/G__rec-2.mp4","continued":true,"part":2,"cause":"size"}), now);
    assert_eq!(view(&reducer), ("Recording continues in part 2".to_string(), "The game's size changed".to_string()));
    // A frame-path change says so, not "size".
    reducer.apply_engine_event(&json!({"type":"recording_started","path":"E:/c/G__rec-3.mp4","continued":true,"part":3,"cause":"input"}), now);
    assert_eq!(view(&reducer), ("Recording continues in part 3".to_string(), "Capture switched between GPU and CPU frames".to_string()));
    let last = json!({"type":"recording_stopped","path":"E:/c/G__rec-3.mp4","seconds":60.0,"bytes":1,"continued":false,"parts":3,"total_seconds":3725.0,"cause":null});
    reducer.apply_engine_event(&last, now);
    assert_eq!(view(&reducer), ("Recording saved".to_string(), "1:02:05 · 3 files".to_string()), "the whole length and the file count");
}
