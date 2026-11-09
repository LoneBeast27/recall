//! The pipe's status payloads: the controller's `session` / `engine` snapshots and what is
//! derived from them (`DaemonSnapshot`).
use std::time::Duration;

use serde::Deserialize;
use serde_json::Value;

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
    #[serde(default)]
    pub setup_owner: Option<u32>,
    /// "waiting" | "starting" | "recording" | "degraded" | "recovering" | "draining" | "paused" | "error" (empty on old builds)
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
    pub paused: bool,
    pub bookmark_hotkey: String,
    pub screenshot_hotkey: String,
    pub record_hotkey: String,
    /// Shows / hides the performance-stats island ("" = off).
    pub stats_hotkey: String,
    /// Turns the overlay's notices on or off ("" = off).
    pub overlay_hotkey: String,
    /// Starts / saves the island editor (place the island per game; "" = off).
    pub editor_hotkey: String,
    /// Hotkeys the recorder could not register, one line each (empty on older builds).
    pub hotkey_problems: Vec<String>,
    pub long_recording: LongRecording,
    /// A setting change waits for a save or long recording to finish.
    pub pending_restart: bool,
    /// The in-game performance-stats island is on.
    pub overlay_stats: bool,
    /// Where the stats island's FPS comes from: "etw" (Windows frame timing) |
    /// "no_permission" (not in Performance Log Users) | "off" (stats off); empty
    /// on builds without it.
    pub fps_source: String,
    /// The closed game's last clip is still saveable until `ends_unix_ms` (state "draining").
    pub drain: Option<Drain>,
    /// When this status arrived (the pipe reader stamps it): `remaining_secs` /
    /// `elapsed_secs` count from here, so a wall-clock jump does not move them.
    #[serde(skip)]
    pub received: Arrival,
    /// A quit waits for a clip still being saved (false on builds without it).
    pub finishing_clip: bool,
}

/// When a status line arrived. Not part of a status's equality: the same status
/// arriving again is no change (no repaint), only a fresher base for the clocks.
#[derive(Debug, Clone, Copy, Default)]
pub struct Arrival(pub Option<std::time::Instant>);

impl PartialEq for Arrival {
    fn eq(&self, _: &Self) -> bool {
        true
    }
}

#[derive(Debug, Clone, Default, Deserialize, PartialEq)]
#[serde(default)]
pub struct Drain {
    pub name: String,
    pub total_secs: u64,
    pub ends_unix_ms: u64,
    /// Seconds left when the status was sent (monotonic; newer controllers).
    #[serde(rename = "remaining_secs")]
    pub remaining: Option<f64>,
}

impl Drain {
    /// Seconds left at `now_unix_ms`, rounded up; 0 once the drain is over.
    pub fn remaining_secs(&self, now_unix_ms: u64) -> u64 {
        self.ends_unix_ms.saturating_sub(now_unix_ms).div_ceil(1000).min(self.total_secs)
    }

    /// Seconds left, rounded up: the status's `remaining_secs` less the time since
    /// it arrived (`since_status`), else the wall-clock end (older controllers).
    pub fn left(&self, now_unix_ms: u64, since_status: Option<Duration>) -> u64 {
        match (self.remaining, since_status) {
            (Some(at_status), Some(since)) => {
                ((at_status - since.as_secs_f64()).max(0.0).ceil() as u64).min(self.total_secs)
            }
            _ => self.remaining_secs(now_unix_ms),
        }
    }
}

/// Milliseconds since the Unix epoch (the controller's drain end uses the same clock).
pub fn unix_ms() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_millis() as u64)
}

#[derive(Debug, Clone, Default, Deserialize, PartialEq)]
#[serde(default)]
pub struct LongRecording {
    pub active: bool,
    pub path: Option<String>,
    /// Unix seconds.
    pub started_secs: f64,
    /// Seconds recorded when the status was sent (monotonic; newer controllers).
    #[serde(rename = "elapsed_secs")]
    pub elapsed: Option<f64>,
}

impl LongRecording {
    /// Seconds recorded: the status's `elapsed_secs` plus the time since it arrived
    /// (`since_status`), else counted from the wall-clock start (older controllers).
    pub fn elapsed_at(&self, now_unix_secs: f64, since_status: Option<Duration>) -> f64 {
        match (self.elapsed, since_status) {
            (Some(at_status), Some(since)) => at_status.max(0.0) + since.as_secs_f64(),
            _ => (now_unix_secs - self.started_secs).max(0.0),
        }
    }
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
    /// "h264_nvenc" | "degraded" (CPU copies) | "recovering" (buffer kept) | "starting" | "off" | "failed"
    pub encoder: String,
    /// "h264" | "hevc" | "av1": what is recording (H.264 after a fallback).
    pub codec: String,
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
    /// Config keys a startup env override sets (`recording.output_dir` from
    /// `Start-Recall.ps1 -OutDir`), from the same event.
    pub config_overridden: Vec<String>,
    /// Shortcuts on a combo games and overlays often use (a warning, they stay bound).
    pub hotkey_warnings: Vec<String>,
    /// Counts `config` events (each one is a fresh answer, even when the config is unchanged).
    pub config_seq: u64,
    /// The last connect was refused: the pipe's server runs as another account (`pipe_owner`).
    pub refused: Option<String>,
}

impl SessionStatus {
    /// Time since this status arrived (`None` for a snapshot built by hand).
    pub fn since_status(&self, now: std::time::Instant) -> Option<Duration> {
        self.received.0.map(|at| now.saturating_duration_since(at))
    }

    /// Drain seconds left now (`Drain::left`).
    pub fn drain_left(&self, now_unix_ms: u64, now: std::time::Instant) -> Option<u64> {
        self.drain.as_ref().map(|d| d.left(now_unix_ms, self.since_status(now)))
    }

    /// A game (or the desktop) is being captured: background cache filling pauses.
    pub fn capturing(&self) -> bool {
        // `degraded`: CPU copies; `recovering`: the engine rebuilds its encoder, buffer kept.
        matches!(self.state.as_str(), "recording" | "starting" | "degraded" | "recovering")
    }
}

impl DaemonSnapshot {
    /// Replay is ready when the buffer holds footage and NVENC runs (also from CPU
    /// copies), or while the engine rebuilds its encoder: the buffer is kept and saveable.
    pub fn replay_ready(&self) -> bool {
        self.engine.as_ref().is_some_and(|e| {
            let running = matches!(e.encoder.as_str(), "h264_nvenc" | "degraded") && e.encoder_error.is_none();
            (running || e.encoder == "recovering") && e.buffer_secs > 0.0
        })
    }
}
