use serde::Serialize;
use serde_json::Value;
use std::path::Path;
use std::time::{Duration, Instant};

#[path = "reducer_save.rs"]
mod save;

use crate::hint::{HintOption, HintView};
use crate::notice::{is_encoder_notice, session_notice, NoticeView, Tone};
use crate::session::Session;

/// The controller's `encoder` notice and the engine's failed status describe
/// the same failure; within this window only the first is shown.
pub const ENCODER_DEDUP: Duration = Duration::from_secs(5);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum EncoderSource {
    Engine,
    Session,
}

pub const STALE_AFTER: Duration = Duration::from_millis(2_500);
const SAVED_FOR: Duration = Duration::from_secs(3);
const FAILURE_FOR: Duration = Duration::from_secs(4);
const MIC_FOR: Duration = Duration::from_secs(2);
/// 2 s (was 1.5 s): from nothing the pill opens before its text shows.
const MARK_FOR: Duration = Duration::from_secs(2);
/// A save in front of the queue with no outcome after this says "Still saving…".
pub const SAVING_FOR: Duration = Duration::from_secs(30);
/// ... and "Save not confirmed" after this (the controller's own `SAVE_STALE`).
pub const CONFIRM_FOR: Duration = Duration::from_secs(120);
/// Controller notices (game picked up, desktop, paused, waiting, encoder).
pub const NOTICE_FOR: Duration = Duration::from_secs(3);
/// The advanced-shortcut hint lives as long as the chord window (the
/// controller's `active:false` closes it earlier).
pub const CHORD_FOR: Duration = Duration::from_millis(1_500);

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
    /// A notice is showing.
    pub visible: bool,
    pub state: RecorderState,
    pub primary: String,
    pub metrics: String,
    pub secondary: String,
    pub connected: bool,
    /// The island's rendering of the notice (`Some` while `visible`, unless
    /// what shows is the shortcut hint).
    pub notice: Option<NoticeView>,
    /// The shortcut hint's options while its window is open (the card).
    pub hint: Option<HintView>,
}

#[derive(Clone, Debug)]
struct Transient {
    primary: String,
    text: String,
    until: Instant,
    /// Replaces the metrics line while shown (the chord hint's first row).
    metrics: Option<String>,
    notice: Option<NoticeView>,
    /// The advanced-shortcut hint (closed by `chord` `active:false`).
    hint: Option<HintView>,
}

#[derive(Debug)]
pub struct Reducer {
    view: OverlayView,
    source: String,
    pub(crate) mic_muted: bool,
    encoder_error: Option<String>,
    last_status: Option<Instant>,
    transient: Option<Transient>,
    /// The metrics line the last status produced (restored after a hint).
    status_metrics: String,
    /// Seconds in the replay buffer (engine `buffer_secs`).
    buffer_secs: u64,
    session: Session,
    /// The last encoder-failure notice shown, and who reported it.
    encoder_shown: Option<(Instant, EncoderSource)>,
    /// Clip saves in flight (`reducer_save.rs`).
    saves: save::Saves,
}

impl Reducer {
    pub fn new() -> Self {
        Self {
            view: resting_view(false),
            source: String::new(),
            mic_muted: false,
            encoder_error: None,
            last_status: None,
            transient: None,
            status_metrics: "Waiting for Recall".into(),
            buffer_secs: 0,
            session: Session::default(),
            encoder_shown: None,
            saves: save::Saves::default(),
        }
    }

    /// True when the other source already showed this encoder failure.
    fn encoder_duplicate(&mut self, source: EncoderSource, now: Instant) -> bool {
        let duplicate = self
            .encoder_shown
            .is_some_and(|(at, from)| from != source && now.saturating_duration_since(at) < ENCODER_DEDUP);
        if !duplicate {
            self.encoder_shown = Some((now, source));
        }
        duplicate
    }

    pub fn view(&self) -> &OverlayView {
        &self.view
    }

    pub fn session(&self) -> &Session {
        &self.session
    }

    pub fn buffer_secs(&self) -> u64 {
        self.buffer_secs
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
        self.last_status = None;
        self.transient = None;
        self.clear_saves();
        self.buffer_secs = 0;
        let session_changed = self.session != Session::default();
        self.session = Session::default();
        self.replace(resting_view(false)) || session_changed
    }

    pub fn apply_envelope(&mut self, value: &Value, now: Instant) -> bool {
        let Some(event) = value.get("event") else {
            return false;
        };
        let kind = event.get("type").and_then(Value::as_str);
        match value.get("mod").and_then(Value::as_str) {
            Some("engine") => self.apply_engine_event(event, now),
            Some("session") => match kind {
                Some("chord") => self.apply_chord(event, now),
                Some("saving") => self.apply_saving(event, now),
                Some("status") => self.session.apply_status(event),
                Some("notice") => {
                    if is_encoder_notice(event) && self.encoder_duplicate(EncoderSource::Session, now) {
                        return false;
                    }
                    let notice = session_notice(event);
                    let (title, detail) = (notice.title.clone(), notice.detail.clone());
                    self.set_transient(&title, detail, now + NOTICE_FOR, notice)
                }
                _ => false,
            },
            _ => false,
        }
    }

    /// `{"type":"chord","active":true,"leader":"Alt `","options":[{"key":"1","label":"Bookmark"},...]}`:
    /// "Recall" over "1 Bookmark · 2 Save 30 s" / "3 Screenshot · 4 Record".
    pub fn apply_chord(&mut self, event: &Value, now: Instant) -> bool {
        if event.get("active").and_then(Value::as_bool) != Some(true) {
            if self.transient.as_ref().is_none_or(|t| t.hint.is_none()) {
                return false; // an action's toast already replaced the hint
            }
            return self.expire_transient() | self.resume_saving(now);
        }
        let options: Vec<HintOption> = event
            .get("options")
            .and_then(Value::as_array)
            .map(|list| {
                list.iter()
                    .filter_map(|o| Some(HintOption { key: o.get("key")?.as_str()?.into(), label: o.get("label")?.as_str()?.into() }))
                    .collect()
            })
            .unwrap_or_default();
        let line = |o: &[HintOption]| o.iter().map(|o| format!("{} {}", o.key, o.label)).collect::<Vec<_>>().join(" · ");
        let split = options.len().div_ceil(2);
        let (first, second) = (line(&options[..split]), line(&options[split..]));
        let leader = event.get("leader").and_then(Value::as_str).unwrap_or("Alt `").to_string();
        let hint = HintView { leader, options };
        let changed = self.set_transient_with("Recall", second, now + CHORD_FOR, None, Some(hint));
        if let Some(t) = self.transient.as_mut() {
            t.metrics = Some(first.clone());
        }
        let metrics_changed = self.view.metrics != first;
        self.view.metrics = first;
        changed || metrics_changed
    }

    pub fn apply_engine_event(&mut self, event: &Value, now: Instant) -> bool {
        use Tone::{Failure, Neutral, Success};
        match event.get("type").and_then(Value::as_str) {
            Some("status") => self.apply_save_progress(event, now) | self.apply_status(event, now),
            Some("mic_muted") => {
                self.mic_muted = event.get("muted").and_then(Value::as_bool).unwrap_or(self.mic_muted);
                let title = if self.mic_muted { "Microphone muted" } else { "Microphone live" };
                let detail = "Game audio continues";
                self.set_transient(title, detail.into(), now + MIC_FOR, NoticeView::new(Neutral, title, detail, false))
            }
            Some("saved") => self.apply_saved(event, now),
            Some("save_failed") => self.apply_save_failed(event, now),
            Some("bookmarked") => {
                let detail = "Marked in the clips you save next";
                let notice = NoticeView::new(Success, "Bookmark added", detail, false).emphasized();
                self.set_transient("Bookmark added", detail.into(), now + MARK_FOR, notice)
            }
            Some("screenshot") => {
                let name = file_name(event);
                let notice = NoticeView::new(Success, "Screenshot saved", name.clone(), false).emphasized();
                self.set_transient("Screenshot saved", format!("Saved {name}"), now + SAVED_FOR, notice)
            }
            // Long recording (`rec_notice`): a continued part's stop shows nothing.
            Some(kind @ ("recording_started" | "recording_stopped")) => match crate::rec_notice::recording_notice(kind, event, file_name(event)) {
                Some(n) => {
                    let notice = NoticeView::new(n.tone, &n.title, n.detail, false).emphasized();
                    self.set_transient(&n.title, n.text, now + SAVED_FOR, notice)
                }
                None => false,
            },
            Some("recording_failed") if crate::rec_notice::failed_notice(event).is_some() => {
                let Some(n) = crate::rec_notice::failed_notice(event) else { return false };
                let notice = NoticeView::new(n.tone, &n.title, n.detail, false).emphasized();
                self.set_transient(&n.title, n.text, now + FAILURE_FOR, notice)
            }
            Some(kind @ ("screenshot_failed" | "recording_failed")) => {
                let reason = truncate(event.get("reason").and_then(Value::as_str).unwrap_or("unknown error"), 48);
                let title = if kind == "screenshot_failed" { "Screenshot failed" } else { "Recording failed" };
                let notice = NoticeView::new(Failure, title, reason.clone(), false).emphasized();
                self.set_transient(title, reason, now + FAILURE_FOR, notice)
            }
            _ => false,
        }
    }

    /// Hide the current toast / hint and restore the resting lines.
    fn expire_transient(&mut self) -> bool {
        let Some(transient) = self.transient.take() else { return false };
        self.view.visible = false;
        self.view.notice = None;
        self.view.hint = None;
        self.view.primary = state_line(&self.view.state).into();
        self.view.secondary = self.source_text();
        if transient.metrics.is_some() {
            self.view.metrics = self.status_metrics.clone();
        }
        true
    }

    pub fn tick(&mut self, now: Instant) -> bool {
        let mut changed = self.expire_saves(now);
        if self.transient.as_ref().is_some_and(|transient| now >= transient.until) {
            changed |= self.expire_transient();
        }
        changed |= self.resume_saving(now);
        if self.last_status.is_some_and(|last| now.saturating_duration_since(last) > STALE_AFTER) {
            changed |= self.mark_stale(now);
        }
        changed
    }

    fn source_text(&self) -> String {
        let source = source_line(&self.source);
        if self.mic_muted { format!("Mic muted | {source}") } else { source }
    }

    fn apply_status(&mut self, event: &Value, now: Instant) -> bool {
        self.mic_muted = event.get("mic_muted").and_then(Value::as_bool).unwrap_or(self.mic_muted);
        self.last_status = Some(now);
        self.view.connected = true;
        self.source = event.get("source").and_then(Value::as_str).unwrap_or("").to_string();

        let encoder = event.get("encoder").and_then(Value::as_str).unwrap_or("starting");
        let encoder_error = event.get("encoder_error").and_then(Value::as_str);
        // "recovering": the engine rebuilds its encoder and keeps the buffer; its error is
        // the reason, not a failure (the session notice says "Encoder problem").
        let failed = encoder == "failed" || (encoder_error.is_some() && encoder != "recovering");
        let new_failure = failed
            && (self.view.state != RecorderState::Failed || self.encoder_error.as_deref() != encoder_error);
        self.encoder_error = encoder_error.map(str::to_owned);
        if new_failure && !self.encoder_duplicate(EncoderSource::Engine, now) {
            let reason = truncate(encoder_error.unwrap_or("NVENC is unavailable"), 48);
            let notice = NoticeView::new(Tone::Failure, "Encoder failed", reason.clone(), false).emphasized();
            self.set_transient("Encoder failed", reason, now + FAILURE_FOR, notice);
        }
        let state = if failed {
            RecorderState::Failed
        } else {
            match encoder {
                // "degraded": recording from CPU copies; replay ready all the same.
                "h264_nvenc" | "degraded" => RecorderState::Ready,
                "starting" | "recovering" => RecorderState::Starting,
                _ => RecorderState::Stopped,
            }
        };
        let buffer = event.get("buffer_secs").and_then(Value::as_f64).unwrap_or(0.0).max(0.0).round() as u64;
        let buffer_changed = self.buffer_secs != buffer;
        self.buffer_secs = buffer;
        let fps = event.get("fps").and_then(Value::as_u64).unwrap_or(0);
        let encoder_label = match encoder {
            "h264_nvenc" => "NVENC",
            "degraded" => "NVENC (CPU copies)",
            other => other,
        };
        self.status_metrics = format!("{buffer}s buffer · Capture {fps} FPS · {encoder_label}");
        let notification = self.active_transient(now);
        let metrics = self
            .transient
            .as_ref()
            .and_then(|t| t.metrics.clone())
            .unwrap_or_else(|| self.status_metrics.clone());
        let secondary = notification
            .clone()
            .or_else(|| encoder_error.map(|error| truncate(error, 48)))
            .unwrap_or_else(|| self.source_text());
        let primary = self
            .transient
            .as_ref()
            .map(|transient| transient.primary.clone())
            .unwrap_or_else(|| state_line(&state).into());
        let notice = self.transient.as_ref().and_then(|t| t.notice.clone());
        let hint = self.transient.as_ref().and_then(|t| t.hint.clone());
        self.replace(OverlayView { visible: notification.is_some(), state, primary, metrics, secondary, connected: true, notice, hint })
            || new_failure
            || buffer_changed
    }

    fn set_transient(&mut self, primary: &str, text: String, until: Instant, notice: NoticeView) -> bool {
        self.set_transient_with(primary, text, until, Some(notice), None)
    }

    /// A notice, or the shortcut hint (an action's notice closes the hint).
    fn set_transient_with(&mut self, primary: &str, text: String, until: Instant, notice: Option<NoticeView>, hint: Option<HintView>) -> bool {
        let had_metrics = self.transient.as_ref().is_some_and(|t| t.metrics.is_some());
        if had_metrics {
            self.view.metrics = self.status_metrics.clone(); // a toast replaces the hint
        }
        self.transient = Some(Transient {
            primary: primary.into(),
            text: text.clone(),
            until,
            metrics: None,
            notice: notice.clone(),
            hint: hint.clone(),
        });
        if self.last_status.is_none() {
            self.view.connected = true;
        }
        let changed = !self.view.visible
            || self.view.primary != primary
            || self.view.secondary != text
            || self.view.notice != notice
            || self.view.hint != hint;
        self.view.visible = true;
        self.view.primary = primary.into();
        self.view.secondary = text;
        self.view.notice = notice;
        self.view.hint = hint;
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
        let transient = self.transient.as_ref();
        self.replace(OverlayView {
            visible: notification.is_some(),
            state: RecorderState::Stopped,
            primary: transient.map(|t| t.primary.clone()).unwrap_or_else(|| "Recorder stopped".into()),
            metrics: "Status feed stale".into(),
            secondary: notification.unwrap_or_else(|| "Waiting for recorder status".into()),
            connected: self.view.connected,
            notice: transient.and_then(|t| t.notice.clone()),
            hint: transient.and_then(|t| t.hint.clone()),
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

fn resting_view(connected: bool) -> OverlayView {
    OverlayView {
        visible: false,
        state: RecorderState::Stopped,
        primary: "Recorder stopped".into(),
        metrics: "Waiting for Recall".into(),
        secondary: "Recorder disconnected".into(),
        connected,
        notice: None,
        hint: None,
    }
}

fn state_line(state: &RecorderState) -> &'static str {
    match state {
        RecorderState::Ready => "Replay ready",
        RecorderState::Starting => "Starting",
        RecorderState::Failed => "Encoder failed",
        RecorderState::Stopped => "Recorder stopped",
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

#[cfg(test)]
#[path = "reducer_save_tests.rs"]
mod save_tests;
