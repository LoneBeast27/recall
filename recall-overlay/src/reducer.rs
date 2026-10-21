use serde::Serialize;
use serde_json::Value;
use std::path::Path;
use std::time::{Duration, Instant};

pub const STALE_AFTER: Duration = Duration::from_millis(2_500);
const SAVED_FOR: Duration = Duration::from_secs(3);
const FAILURE_FOR: Duration = Duration::from_secs(4);
const MIC_FOR: Duration = Duration::from_secs(2);

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RecorderState {
    Ready,
    Starting,
    Failed,
    Stopped,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct OverlayView {
    pub visible: bool,
    pub state: RecorderState,
    pub primary: String,
    pub metrics: String,
    pub secondary: String,
    pub connected: bool,
}

#[derive(Clone, Debug)]
struct Transient {
    primary: String,
    text: String,
    until: Instant,
}

#[derive(Debug)]
pub struct Reducer {
    view: OverlayView,
    source: String,
    mic_muted: bool,
    encoder_error: Option<String>,
    last_status: Option<Instant>,
    transient: Option<Transient>,
}

impl Reducer {
    pub fn new() -> Self {
        Self {
            view: OverlayView {
                visible: false,
                state: RecorderState::Stopped,
                primary: "Recorder stopped".into(),
                metrics: "Waiting for Recall".into(),
                secondary: "Recorder disconnected".into(),
                connected: false,
            },
            source: String::new(),
            mic_muted: false,
            encoder_error: None,
            last_status: None,
            transient: None,
        }
    }

    pub fn view(&self) -> &OverlayView {
        &self.view
    }

    pub fn last_status_age(&self, now: Instant) -> Option<Duration> {
        self.last_status.map(|at| now.saturating_duration_since(at))
    }

    pub fn connected(&mut self) -> bool {
        if self.view.connected {
            return false;
        }
        self.view.connected = true;
        true
    }

    pub fn disconnected(&mut self) -> bool {
        let next = OverlayView {
            visible: false,
            state: RecorderState::Stopped,
            primary: "Recorder stopped".into(),
            metrics: "Waiting for Recall".into(),
            secondary: "Recorder disconnected".into(),
            connected: false,
        };
        self.last_status = None;
        self.transient = None;
        self.replace(next)
    }

    pub fn apply_envelope(&mut self, value: &Value, now: Instant) -> bool {
        if value.get("mod").and_then(Value::as_str) != Some("engine") {
            return false;
        }
        let Some(event) = value.get("event") else {
            return false;
        };
        self.apply_engine_event(event, now)
    }

    pub fn apply_engine_event(&mut self, event: &Value, now: Instant) -> bool {
        match event.get("type").and_then(Value::as_str) {
            Some("status") => self.apply_status(event, now),
            Some("mic_muted") => {
                self.mic_muted=event.get("muted").and_then(Value::as_bool).unwrap_or(self.mic_muted);
                let title = if self.mic_muted { "Microphone muted" } else { "Microphone live" };
                self.set_transient(title, "Game audio continues".into(), now + MIC_FOR)
            }
            Some("saved") => {
                let filename = event
                    .get("path")
                    .and_then(Value::as_str)
                    .and_then(|p| Path::new(p).file_name())
                    .and_then(|p| p.to_str())
                    .unwrap_or("clip");
                self.set_transient("Clip saved", format!("Saved {filename}"), now + SAVED_FOR)
            }
            Some("save_failed") => {
                let reason = event
                    .get("reason")
                    .and_then(Value::as_str)
                    .unwrap_or("unknown error");
                self.set_transient(
                    "Save failed",
                    format!("Save failed: {}", truncate(reason, 44)),
                    now + FAILURE_FOR,
                )
            }
            _ => false,
        }
    }

    pub fn tick(&mut self, now: Instant) -> bool {
        let mut changed = false;
        if self.transient.as_ref().is_some_and(|transient| now >= transient.until) {
            self.transient = None;
            self.view.visible = false;
            self.view.primary = match self.view.state {
                RecorderState::Ready => "Replay ready",
                RecorderState::Starting => "Starting",
                RecorderState::Failed => "Encoder failed",
                RecorderState::Stopped => "Recorder stopped",
            }.into();
            self.view.secondary = self.source_text();
            changed = true;
        }
        if self.last_status.is_some_and(|last| now.saturating_duration_since(last) > STALE_AFTER) {
            changed |= self.mark_stale(now);
        }
        changed
    }

    fn source_text(&self) -> String {
        let source=source_line(&self.source);
        if self.mic_muted { format!("Mic muted | {source}") } else { source }
    }
    fn apply_status(&mut self, event: &Value, now: Instant) -> bool {
        self.mic_muted=event.get("mic_muted").and_then(Value::as_bool).unwrap_or(self.mic_muted);
        self.last_status = Some(now);
        self.view.connected = true;
        self.source = event
            .get("source")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();

        let encoder = event
            .get("encoder")
            .and_then(Value::as_str)
            .unwrap_or("starting");
        let encoder_error = event.get("encoder_error").and_then(Value::as_str);
        let new_failure = (encoder_error.is_some() || encoder == "failed")
            && (self.view.state != RecorderState::Failed || self.encoder_error.as_deref() != encoder_error);
        self.encoder_error = encoder_error.map(str::to_owned);
        if new_failure {
            self.set_transient("Encoder failed", truncate(encoder_error.unwrap_or("NVENC is unavailable"), 48), now + FAILURE_FOR);
        }
        let (state, primary) = if encoder_error.is_some() || encoder == "failed" {
            (RecorderState::Failed, "Encoder failed")
        } else {
            match encoder {
                "h264_nvenc" => (RecorderState::Ready, "Replay ready"),
                "starting" => (RecorderState::Starting, "Starting"),
                _ => (RecorderState::Stopped, "Recorder stopped"),
            }
        };
        let buffer = event
            .get("buffer_secs")
            .and_then(Value::as_f64)
            .unwrap_or(0.0)
            .max(0.0)
            .round() as u64;
        let fps = event.get("fps").and_then(Value::as_u64).unwrap_or(0);
        let encoder_label = if encoder == "h264_nvenc" {
            "NVENC"
        } else {
            encoder
        };
        let metrics = format!("{buffer}s buffer · Capture {fps} FPS · {encoder_label}");
        let notification = self.active_transient(now);
        let secondary = notification.clone()
            .or_else(|| encoder_error.map(|error| truncate(error, 48)))
            .unwrap_or_else(|| self.source_text());
        let primary = self.transient.as_ref().map(|transient| transient.primary.clone()).unwrap_or_else(|| primary.into());
        self.replace(OverlayView {
            visible: notification.is_some(),
            state,
            primary,
            metrics,
            secondary,
            connected: true,
        }) || new_failure
    }

    fn set_transient(&mut self, primary: &str, text: String, until: Instant) -> bool {
        self.transient = Some(Transient { primary: primary.into(), text: text.clone(), until });
        if self.last_status.is_none() {
            self.view.connected = true;
        }
        let changed = !self.view.visible || self.view.primary != primary || self.view.secondary != text;
        self.view.visible = true;
        self.view.primary = primary.into();
        self.view.secondary = text;
        changed
    }

    fn active_transient(&mut self, now: Instant) -> Option<String> {
        match self.transient.as_ref() {
            Some(transient) if now < transient.until => Some(transient.text.clone()),
            Some(_) => {
                self.transient = None;
                None
            }
            None => None,
        }
    }

    fn mark_stale(&mut self, now: Instant) -> bool {
        self.last_status = None;
        let notification = self.active_transient(now);
        self.replace(OverlayView {
            visible: notification.is_some(),
            state: RecorderState::Stopped,
            primary: self.transient.as_ref().map(|transient| transient.primary.clone()).unwrap_or_else(|| "Recorder stopped".into()),
            metrics: "Status feed stale".into(),
            secondary: notification.unwrap_or_else(|| "Waiting for recorder status".into()),
            connected: self.view.connected,
        })
    }

    fn replace(&mut self, next: OverlayView) -> bool {
        if self.view == next {
            false
        } else {
            self.view = next;
            true
        }
    }
}

fn source_line(source: &str) -> String {
    if source.trim().is_empty() {
        "Waiting for a capture source".into()
    } else {
        format!("Source: {}", truncate(source.trim(), 42))
    }
}

pub fn truncate(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        return text.to_string();
    }
    let mut short: String = text.chars().take(max_chars.saturating_sub(1)).collect();
    short.push('…');
    short
}

#[cfg(test)]
mod tests {
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

    #[test]
    fn disconnect_clears_ready_state_immediately() {
        let now = Instant::now();
        let mut reducer = Reducer::new();
        reducer.apply_envelope(&status("h264_nvenc"), now);
        assert!(reducer.disconnected());
        assert_eq!(reducer.view().state, RecorderState::Stopped);
        assert!(!reducer.view().connected);
    }
}
