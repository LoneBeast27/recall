//! Status parts the controller sends (split out of `daemon.rs`): when a line arrived, a
//! drain, a long recording, and the engine's own status.

use std::time::Duration;

use serde::Deserialize;

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
    /// What each recorded source captures (`status.audio`); empty from an engine before it.
    pub audio: Vec<EngineAudio>,
}

/// One source in `status.audio`. `held`: the engine stores silence for it whatever Alt+M says
/// (an unsure sibling, or the startup hold); None from an engine that does not say
/// (merge-gate P2: the meter then guesses from the device lists, as before).
#[derive(Debug, Clone, Default, Deserialize, PartialEq)]
#[serde(default)]
pub struct EngineAudio {
    pub id: String,
    pub held: Option<bool>,
}
