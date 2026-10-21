use serde::Serialize;
use serde_json::Value;
use std::path::Path;
use std::time::{Duration, Instant};

pub const STALE_AFTER: Duration = Duration::from_millis(2_500);
const SAVED_FOR: Duration = Duration::from_secs(3);
const FAILURE_FOR: Duration = Duration::from_secs(4);
const MIC_FOR: Duration = Duration::from_secs(2);
const MARK_FOR: Duration = Duration::from_millis(1_500);
/// The advanced-shortcut hint lives as long as the chord window (the
/// controller's `active:false` closes it earlier).
const CHORD_FOR: Duration = Duration::from_millis(1_500);

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
    /// Replaces the metrics line while shown (the chord hint's first row).
    metrics: Option<String>,
    /// The advanced-shortcut hint (closed by `chord` `active:false`).
    chord: bool,
}

#[derive(Debug)]
pub struct Reducer {
    view: OverlayView,
    source: String,
    mic_muted: bool,
    encoder_error: Option<String>,
    last_status: Option<Instant>,
    transient: Option<Transient>,
    /// The metrics line the last status produced (restored after a hint).
    status_metrics: String,
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
            status_metrics: "Waiting for Recall".into(),
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
        let Some(event) = value.get("event") else {
            return false;
        };
        match value.get("mod").and_then(Value::as_str) {
            Some("engine") => self.apply_engine_event(event, now),
            Some("session") if event.get("type").and_then(Value::as_str) == Some("chord") => self.apply_chord(event, now),
            _ => false,
        }
    }

    /// `{"type":"chord","active":true,"options":[{"key":"1","label":"Bookmark"},...]}`:
    /// "Recall" over "1 Bookmark · 2 Save 30 s" / "3 Screenshot · 4 Record".
    pub fn apply_chord(&mut self, event: &Value, now: Instant) -> bool {
        if event.get("active").and_then(Value::as_bool) != Some(true) {
            if !self.transient.as_ref().is_some_and(|t| t.chord) {
                return false; // an action's toast already replaced the hint
            }
            return self.expire_transient();
        }
        let options: Vec<String> = event
            .get("options")
            .and_then(Value::as_array)
            .map(|list| {
                list.iter()
                    .filter_map(|o| Some(format!("{} {}", o.get("key")?.as_str()?, o.get("label")?.as_str()?)))
                    .collect()
            })
            .unwrap_or_default();
        let split = options.len().div_ceil(2);
        let (first, second) = (options[..split].join(" · "), options[split..].join(" · "));
        let changed = self.set_transient("Recall", second, now + CHORD_FOR);
        if let Some(t) = self.transient.as_mut() {
            t.metrics = Some(first.clone());
            t.chord = true;
        }
        let metrics_changed = self.view.metrics != first;
        self.view.metrics = first;
        changed || metrics_changed
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
                let game = filename.split("__").next().unwrap_or(filename).replace(['-', '_'], " ");
                let secs = event.get("seconds").and_then(Value::as_f64).unwrap_or(0.0).round() as u64;
                let detail = if secs > 0 { format!("{game} · {}:{:02}", secs / 60, secs % 60) } else { game };
                self.set_transient("Clip saved", detail, now + SAVED_FOR)
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
            Some("bookmarked") => self.set_transient("Bookmark added", "Marked in the clips you save next".into(), now + MARK_FOR),
            Some("screenshot") => {
                let text = format!("Saved {}", file_name(event));
                self.set_transient("Screenshot saved", text, now + SAVED_FOR)
            }
            Some("recording_started") => {
                let text = format!("Writing {}", file_name(event));
                self.set_transient("Recording started", text, now + SAVED_FOR)
            }
            Some("recording_stopped") => {
                let secs = event.get("seconds").and_then(Value::as_f64).unwrap_or(0.0).max(0.0) as u64;
                let text = format!("{} · {}:{:02}", file_name(event), secs / 60, secs % 60);
                self.set_transient("Recording saved", truncate(&text, 48), now + SAVED_FOR)
            }
            Some(kind @ ("screenshot_failed" | "recording_failed")) => {
                let reason = event.get("reason").and_then(Value::as_str).unwrap_or("unknown error");
                let title = if kind == "screenshot_failed" { "Screenshot failed" } else { "Recording failed" };
                self.set_transient(title, truncate(reason, 48), now + FAILURE_FOR)
            }
            _ => false,
        }
    }

    /// Hide the current toast / hint and restore the resting lines.
    fn expire_transient(&mut self) -> bool {
        let Some(transient) = self.transient.take() else { return false };
        self.view.visible = false;
        self.view.primary = match self.view.state {
            RecorderState::Ready => "Replay ready",
            RecorderState::Starting => "Starting",
            RecorderState::Failed => "Encoder failed",
            RecorderState::Stopped => "Recorder stopped",
        }
        .into();
        self.view.secondary = self.source_text();
        if transient.metrics.is_some() {
            self.view.metrics = self.status_metrics.clone();
        }
        true
    }

    pub fn tick(&mut self, now: Instant) -> bool {
        let mut changed = false;
        if self.transient.as_ref().is_some_and(|transient| now >= transient.until) {
            changed = self.expire_transient();
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
        self.status_metrics = format!("{buffer}s buffer · Capture {fps} FPS · {encoder_label}");
        let notification = self.active_transient(now);
        let metrics = self
            .transient
            .as_ref()
            .and_then(|t| t.metrics.clone())
            .unwrap_or_else(|| self.status_metrics.clone());
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
        let had_metrics = self.transient.as_ref().is_some_and(|t| t.metrics.is_some());
        if had_metrics {
            self.view.metrics = self.status_metrics.clone(); // a toast replaces the hint
        }
        self.transient = Some(Transient { primary: primary.into(), text: text.clone(), until, metrics: None, chord: false });
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

/// The event's `path` file name, shortened for the card.
fn file_name(event: &Value) -> String {
    let name = event
        .get("path")
        .and_then(Value::as_str)
        .and_then(|p| Path::new(p).file_name())
        .and_then(|p| p.to_str())
        .unwrap_or("file");
    truncate(name, 40)
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
#[path = "reducer_tests.rs"]
mod tests;
