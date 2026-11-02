//! Pure pipe-protocol logic: parse `{"mod":ns,"event":{..}}` lines into the
//! snapshot / event queue, and serialize commands. No I/O here.
use serde::de::DeserializeOwned;
use serde_json::{Map, Value, json};

use super::{DaemonCommand, DaemonEvent, DaemonSnapshot, EngineStatus, HdrStatus, SessionStatus};

/// Events the UI has not drained yet are capped so an idle UI cannot grow memory.
const MAX_QUEUED_EVENTS: usize = 256;

#[derive(Debug, Default)]
pub(super) struct State {
    pub snapshot: DaemonSnapshot,
    pub events: Vec<DaemonEvent>,
    /// Merged raw `session` fields. Old builds send a subset, and `hdr`/`mic_muted`
    /// updates arrive from other namespaces, so keys absent from a new status keep
    /// their last value instead of resetting.
    session_raw: Option<Map<String, Value>>,
}

impl State {
    /// Applies one pipe line; true when the snapshot changed or an event was queued.
    pub fn apply_line(&mut self, line: &str) -> bool {
        match serde_json::from_str::<Value>(line.trim()) {
            Ok(value) => self.apply_envelope(&value),
            Err(_) => false,
        }
    }

    /// A disconnect drops the live status (it would be stale); the last config stays
    /// so settings can still be shown while the controller restarts.
    pub fn set_connected(&mut self, connected: bool) -> bool {
        let before = self.snapshot.clone();
        self.snapshot.connected = connected;
        if !connected {
            self.snapshot.session = None;
            self.snapshot.engine = None;
            self.session_raw = None;
        }
        self.snapshot != before
    }

    pub fn apply_envelope(&mut self, value: &Value) -> bool {
        let namespace = value.get("mod").and_then(Value::as_str).unwrap_or("");
        let Some(event) = value.get("event").and_then(Value::as_object) else { return false };
        let kind = event.get("type").and_then(Value::as_str).unwrap_or("");
        let before = self.snapshot.clone();
        let queued = self.events.len();
        match (namespace, kind) {
            ("session", "status") => self.merge_session(event),
            ("session", "config") => self.snapshot.config = Some(config_of(event)),
            ("session", "storage_cleaned") => self.push(DaemonEvent::StorageCleaned { removed: u64_of(event, "removed"), freed_bytes: u64_of(event, "freed_bytes") }),
            ("engine", "status") => self.engine_status(event),
            ("engine", "hello") => self.snapshot.engine = Some(EngineStatus { encoder: "starting".into(), ..EngineStatus::default() }),
            ("hdr", "status") => self.hdr_status(event),
            ("hdr", "ac_profile") => self.push(DaemonEvent::AcProfile {
                ok: bool_of(event, "ok"),
                profile: string_of(event, "profile"),
                error: string_of(event, "error"),
            }),
            (_, "mic_muted") => {
                let muted = bool_of(event, "muted");
                self.set_mic_muted(muted);
                self.push(DaemonEvent::MicMuted(muted));
            }
            (_, "error") => self.push(DaemonEvent::Error(error_text(event))),
            ("engine", _) => self.engine_event(kind, event),
            _ => {}
        }
        self.snapshot != before || self.events.len() != queued
    }

    fn push(&mut self, event: DaemonEvent) {
        if self.events.len() >= MAX_QUEUED_EVENTS {
            self.events.remove(0);
        }
        self.events.push(event);
    }

    fn engine_event(&mut self, kind: &str, event: &Map<String, Value>) {
        let source = || string_of(event, "source").unwrap_or_default();
        let path = || string_of(event, "path").unwrap_or_default();
        let fps = || u64_of(event, "fps").min(u32::MAX as u64) as u32;
        let parsed = match kind {
            "saved" => DaemonEvent::Saved {
                path: path(),
                seconds: f64_of(event, "seconds") as f32,
                bytes: u64_of(event, "bytes"),
                elapsed_ms: u64_of(event, "elapsed_ms"),
            },
            "save_failed" => DaemonEvent::SaveFailed { reason: string_of(event, "reason").unwrap_or_else(|| "Save failed.".into()) },
            "export_started" => DaemonEvent::ExportStarted { source: source(), path: path(), fps: fps() },
            "exported" => DaemonEvent::Exported { source: source(), path: path(), fps: fps(), frames: u64_of(event, "frames"), seconds: f64_of(event, "seconds") },
            "export_failed" => DaemonEvent::ExportFailed { source: source(), reason: string_of(event, "reason").unwrap_or_else(|| "Export failed.".into()) },
            "bookmarked" => DaemonEvent::Bookmarked,
            "screenshot" => DaemonEvent::Screenshot { path: path() },
            "screenshot_failed" => DaemonEvent::ScreenshotFailed { reason: string_of(event, "reason").unwrap_or_else(|| "Screenshot failed.".into()) },
            "recording_started" => DaemonEvent::RecordingStarted,
            "clip_moved" => DaemonEvent::ClipMoved { path: path(), kept_path: string_of(event, "kept_path") },
            "clip_move_failed" => DaemonEvent::ClipMoveFailed { path: path(), reason: string_of(event, "reason").unwrap_or_default() },
            "recording_stopped" => DaemonEvent::RecordingStopped { path: path(), seconds: f64_of(event, "seconds") },
            "recording_failed" => DaemonEvent::RecordingFailed { reason: string_of(event, "reason").unwrap_or_else(|| "Recording failed.".into()) },
            // encoder_log and unknown engine lines carry nothing the UI shows.
            _ => return,
        };
        self.push(parsed);
    }

    fn engine_status(&mut self, event: &Map<String, Value>) {
        let status: EngineStatus = lenient(event);
        // The engine owns microphone truth; keep the session mirror in step.
        if event.contains_key("mic_muted") {
            self.raw_session().insert("mic_muted".into(), Value::Bool(status.mic_muted));
            self.reparse_session();
        }
        self.snapshot.engine = Some(status);
    }

    fn set_mic_muted(&mut self, muted: bool) {
        if let Some(engine) = self.snapshot.engine.as_mut() {
            engine.mic_muted = muted;
        }
        self.raw_session().insert("mic_muted".into(), Value::Bool(muted));
        self.reparse_session();
    }

    fn merge_session(&mut self, event: &Map<String, Value>) {
        let raw = self.raw_session();
        for (key, value) in event {
            if key != "type" {
                raw.insert(key.clone(), value.clone());
            }
        }
        self.reparse_session();
    }

    /// New controllers send `HdrStatus` fields; the legacy HDR daemon sends
    /// `{target, hdr, manual_pin, session_pid, auto, engine_health}`.
    fn hdr_status(&mut self, event: &Map<String, Value>) {
        let hdr = if event.contains_key("hdr_on") || event.contains_key("available") {
            let mut fields = event.clone();
            fields.remove("type");
            lenient::<HdrStatus>(&fields)
        } else {
            HdrStatus {
                available: true,
                display: string_of(event, "target").filter(|name| !name.is_empty()),
                hdr_on: bool_of(event, "hdr"),
                manual_pin: bool_of(event, "manual_pin"),
                auto: bool_of(event, "auto"),
            }
        };
        let raw = self.raw_session();
        raw.insert("hdr".into(), json!({"available": hdr.available, "display": hdr.display, "hdr_on": hdr.hdr_on, "manual_pin": hdr.manual_pin, "auto": hdr.auto}));
        if let Some(health) = event.get("engine_health").and_then(Value::as_str) {
            raw.insert("engine_health".into(), Value::String(health.into()));
        }
        self.reparse_session();
    }

    fn raw_session(&mut self) -> &mut Map<String, Value> {
        self.session_raw.get_or_insert_with(Map::new)
    }

    fn reparse_session(&mut self) {
        self.snapshot.session = self.session_raw.as_ref().map(lenient::<SessionStatus>);
    }
}

/// Deserializes `T`, dropping any field whose value has the wrong shape instead of
/// failing the whole object (old controllers, nulls, future type changes).
pub(super) fn lenient<T: DeserializeOwned + Default>(fields: &Map<String, Value>) -> T {
    if let Ok(value) = serde_json::from_value(Value::Object(fields.clone())) {
        return value;
    }
    let mut kept = Map::new();
    for (key, value) in fields {
        let single = Map::from_iter([(key.clone(), value.clone())]);
        if serde_json::from_value::<T>(Value::Object(single)).is_ok() {
            kept.insert(key.clone(), value.clone());
        }
    }
    serde_json::from_value(Value::Object(kept)).unwrap_or_default()
}

fn config_of(event: &Map<String, Value>) -> Value {
    match event.get("config") {
        Some(config) => config.clone(),
        None => {
            let mut rest = event.clone();
            rest.remove("type");
            Value::Object(rest)
        }
    }
}

fn error_text(event: &Map<String, Value>) -> String {
    ["message", "error", "reason"]
        .iter()
        .find_map(|key| string_of(event, key))
        .unwrap_or_else(|| Value::Object(event.clone()).to_string())
}

fn string_of(event: &Map<String, Value>, key: &str) -> Option<String> {
    event.get(key).and_then(Value::as_str).map(str::to_owned)
}

fn bool_of(event: &Map<String, Value>, key: &str) -> bool {
    event.get(key).and_then(Value::as_bool).unwrap_or(false)
}

fn u64_of(event: &Map<String, Value>, key: &str) -> u64 {
    event.get(key).and_then(|v| v.as_u64().or_else(|| v.as_f64().filter(|f| *f >= 0.0).map(|f| f as u64))).unwrap_or(0)
}

fn f64_of(event: &Map<String, Value>, key: &str) -> f64 {
    event.get(key).and_then(Value::as_f64).unwrap_or(0.0)
}

/// One command as the single-line JSON object the controller reads.
pub(super) fn command_line(command: &DaemonCommand) -> String {
    let value = match command {
        DaemonCommand::Status => json!({"cmd": "status"}),
        DaemonCommand::Quit => json!({"cmd": "quit"}),
        DaemonCommand::SaveClip { seconds: None } => json!({"cmd": "save_clip"}),
        DaemonCommand::SaveClip { seconds: Some(seconds) } => json!({"cmd": "save_clip", "seconds": seconds}),
        DaemonCommand::SetMicMute(muted) => json!({"cmd": "set_mic_mute", "muted": muted}),
        DaemonCommand::ToggleMicMute => json!({"cmd": "toggle_mic_mute"}),
        DaemonCommand::Pause => json!({"cmd": "pause"}),
        DaemonCommand::Resume => json!({"cmd": "resume"}),
        DaemonCommand::Bookmark => json!({"cmd": "bookmark"}),
        DaemonCommand::Screenshot => json!({"cmd": "screenshot"}),
        DaemonCommand::RecordToggle => json!({"cmd": "record_toggle"}),
        DaemonCommand::ToggleHdr => json!({"cmd": "toggle_hdr"}),
        DaemonCommand::ToggleStats => json!({"cmd": "toggle_stats"}),
        DaemonCommand::SetMode(mode) => json!({"cmd": "set_mode", "value": mode}),
        DaemonCommand::SetAcProfile(variant) => json!({"cmd": "set_ac_profile", "variant": variant}),
        DaemonCommand::GetConfig => json!({"cmd": "get_config"}),
        DaemonCommand::SetConfig(patch) => json!({"cmd": "set_config", "patch": patch}),
    };
    value.to_string()
}
