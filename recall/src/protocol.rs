//! Machine-readable JSON protocol spoken over stdout/stdin when `--json` is set.
//!
//! The Tauri control panel spawns the engine as a sidecar, parses these
//! newline-delimited JSON messages from stdout, and writes `Command`s to stdin.
//! Without `--json`, the engine prints human `[recall]` logs instead.

use serde::{Deserialize, Serialize};

/// Messages the engine emits on stdout (one JSON object per line).
#[derive(Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Outbound {
    /// Sent once at startup: version + available monitors.
    Hello {
        version: String,
        monitors: Vec<MonitorInfo>,
    },
    /// Sent on change (and ~1/s for fps): current live state.
    Status {
        /// "hdr" | "sdr" — the detected display color space.
        display: String,
        max_nits: f32,
        /// Effective tonemap mode: "passthrough" | "tonemap".
        mode: String,
        /// Mode selector: "auto" | "sdr" | "hdr".
        select: String,
        /// Current `<source>` (without the " SDR" suffix).
        source: String,
        width: u32,
        height: u32,
        fps: u32,
    },
}

#[derive(Serialize)]
pub struct MonitorInfo {
    pub index: usize,
    pub name: String,
    pub width: u32,
    pub height: u32,
    pub primary: bool,
}

/// Commands the engine accepts on stdin (one JSON object per line).
#[derive(Deserialize)]
#[serde(tag = "cmd", rename_all = "snake_case")]
pub enum Command {
    /// value: "auto" | "sdr" | "hdr"
    SetMode { value: String },
    /// Ask the engine to exit cleanly.
    Quit,
}

/// Serialize and print one outbound message as a JSON line.
pub fn emit(msg: &Outbound) {
    if let Ok(line) = serde_json::to_string(msg) {
        println!("{line}");
    }
}
