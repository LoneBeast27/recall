//! Client for the controller's control pipe (`\\.\pipe\recall-hdr-control`).
//!
//! A background thread connects, reconnects every second, parses newline JSON
//! `{"mod":ns,"event":{..}}` messages into a shared snapshot plus a queue of
//! one-shot events. The UI polls `revision()` / `snapshot()` / `drain_events()`.
use serde::Deserialize;
use serde_json::Value;

#[derive(Debug, Clone)]
pub enum DaemonCommand {
    Status,
    Quit,
    SaveClip { seconds: Option<u32> },
    SetMicMute(bool),
    ToggleMicMute,
    ToggleHdr,
    /// "auto" | "sdr" | "hdr"
    SetMode(String),
    /// "punchy" | "accurate"
    SetAcProfile(String),
    GetConfig,
    /// Partial config object with the TOML section structure, e.g. {"overlay":{"enabled":true}}.
    SetConfig(Value),
}

#[derive(Debug, Clone, Default, Deserialize, PartialEq)]
#[serde(default)]
pub struct Target {
    pub name: String,
    pub exe: String,
    pub pid: u32,
}

#[derive(Debug, Clone, Default, Deserialize, PartialEq)]
#[serde(default)]
pub struct OverlayStatus {
    pub enabled: bool,
    pub corner: String,
    pub running: bool,
}

#[derive(Debug, Clone, Default, Deserialize, PartialEq)]
#[serde(default)]
pub struct HdrStatus {
    pub available: bool,
    pub display: Option<String>,
    pub hdr_on: bool,
    pub manual_pin: bool,
    pub auto: bool,
}

/// `session` status from the controller. Lenient: older record-only builds send a subset.
#[derive(Debug, Clone, Default, Deserialize, PartialEq)]
#[serde(default)]
pub struct SessionStatus {
    /// "waiting" | "starting" | "recording" | "error" (empty on old builds)
    pub state: String,
    pub target: Option<Target>,
    pub engine_health: String,
    pub auto_record: bool,
    pub save_hotkey: String,
    pub mic_hotkey: String,
    pub save_seconds: u32,
    pub mic_muted: bool,
    pub overlay: OverlayStatus,
    pub autostart: bool,
    pub hdr: HdrStatus,
}

/// `engine` status (about once per second while the recorder runs).
#[derive(Debug, Clone, Default, Deserialize, PartialEq)]
#[serde(default)]
pub struct EngineStatus {
    pub source: String,
    pub width: u32,
    pub height: u32,
    /// Encoded capture fps, not game fps.
    pub fps: u32,
    pub buffer_secs: f32,
    /// "h264_nvenc" | "starting" | "off" | "failed"
    pub encoder: String,
    pub encoder_error: Option<String>,
    pub mic_muted: bool,
    /// "hdr" | "sdr"
    pub display: String,
    /// "passthrough" | "tonemap"
    pub mode: String,
    /// "auto" | "sdr" | "hdr"
    pub select: String,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct DaemonSnapshot {
    pub connected: bool,
    pub session: Option<SessionStatus>,
    pub engine: Option<EngineStatus>,
    /// Full config object from the last `config` event.
    pub config: Option<Value>,
}

impl DaemonSnapshot {
    /// Replay is ready when NVENC runs and the buffer holds footage.
    pub fn replay_ready(&self) -> bool {
        self.engine
            .as_ref()
            .is_some_and(|e| e.encoder == "h264_nvenc" && e.buffer_secs > 0.0 && e.encoder_error.is_none())
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum DaemonEvent {
    Saved { path: String, seconds: f32, bytes: u64, elapsed_ms: u64 },
    SaveFailed { reason: String },
    MicMuted(bool),
    ExportStarted { source: String, path: String, fps: u32 },
    Exported { source: String, path: String, fps: u32, frames: u64, seconds: f64 },
    ExportFailed { source: String, reason: String },
    AcProfile { ok: bool, profile: Option<String>, error: Option<String> },
    Error(String),
}

pub struct DaemonClient {
    _private: (),
}

impl DaemonClient {
    /// Starts the background connection thread. Never blocks.
    pub fn spawn() -> Self {
        Self { _private: () }
    }
    /// Queues a command; returns false when not connected (the command is dropped).
    pub fn send(&self, _command: DaemonCommand) -> bool {
        false
    }
    /// Increments whenever the snapshot changes or an event arrives.
    pub fn revision(&self) -> u64 {
        0
    }
    pub fn snapshot(&self) -> DaemonSnapshot {
        DaemonSnapshot::default()
    }
    pub fn drain_events(&self) -> Vec<DaemonEvent> {
        Vec::new()
    }
}

/// Launches the controller (`recall-hdr.exe --record`, hidden, CCD1 affinity inherited)
/// when no controller is running. Ok(false) when one is already running.
pub fn start_controller() -> Result<bool, String> {
    Err("controller launch not implemented".into())
}
