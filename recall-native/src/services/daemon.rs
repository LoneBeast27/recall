//! Client for the controller's control pipe (`\\.\pipe\recall-hdr-control`).
//!
//! A background thread connects, reconnects every second, parses newline JSON
//! `{"mod":ns,"event":{..}}` messages into a shared snapshot plus a queue of
//! one-shot events. The UI polls `revision()` / `snapshot()` / `drain_events()`.
use std::io::{BufRead, BufReader, Write};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use interprocess::local_socket::traits::Stream as _;
use interprocess::local_socket::{GenericNamespaced, Stream, ToNsName};
use serde::Deserialize;
use serde_json::Value;

mod controller;
mod protocol;
#[cfg(test)]
mod tests;
#[cfg(test)]
mod tests_pipe;

const PIPE_NAME: &str = "recall-hdr-control";
const RECONNECT: Duration = Duration::from_secs(1);
/// A `status` request this often: only the controller's pump answers it, so the
/// replies show it is alive (engine lines come from another thread).
const PING: Duration = Duration::from_secs(2);
/// No `session` status for this long on an open pipe = the recorder is not responding.
pub const UNRESPONSIVE: Duration = Duration::from_secs(5);

#[derive(Debug, Clone)]
pub enum DaemonCommand {
    Status,
    Quit,
    SaveClip { seconds: Option<u32> },
    SetMicMute(bool),
    ToggleMicMute,
    /// Stop capturing entirely until Resume (runtime only).
    Pause,
    Resume,
    /// Marks this moment; the next save carries the bookmark.
    Bookmark,
    Screenshot,
    /// Starts or stops the manual long recording.
    RecordToggle,
    ToggleHdr,
    /// Shows or hides the in-game performance-stats island (`overlay.stats`, saved).
    ToggleStats,
    /// "auto" | "sdr" | "hdr"
    SetMode(String),
    /// "punchy" | "accurate"
    SetAcProfile(String),
    GetConfig,
    /// Partial config object with the TOML section structure, e.g. {"overlay":{"enabled":true}}.
    SetConfig(Value),
    /// What `storage.cap_gb` = this would move to the Recycle Bin now (answered by a
    /// `StoragePreview` event; nothing moves).
    StoragePreview(u32),
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
}

#[derive(Debug, Clone, Default, Deserialize, PartialEq)]
#[serde(default)]
pub struct Drain {
    pub name: String,
    pub total_secs: u64,
    pub ends_unix_ms: u64,
}

impl Drain {
    /// Seconds left at `now_unix_ms`, rounded up; 0 once the drain is over.
    pub fn remaining_secs(&self, now_unix_ms: u64) -> u64 {
        self.ends_unix_ms.saturating_sub(now_unix_ms).div_ceil(1000).min(self.total_secs)
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
}

impl SessionStatus {
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

#[derive(Debug, Clone, PartialEq)]
pub enum DaemonEvent {
    Saved { path: String, seconds: f32, bytes: u64, elapsed_ms: u64 },
    SaveFailed { reason: String },
    MicMuted(bool),
    ExportStarted { source: String, path: String, fps: u32 },
    Exported { source: String, path: String, fps: u32, frames: u64, seconds: f64 },
    ExportFailed { source: String, reason: String },
    AcProfile { ok: bool, profile: Option<String>, error: Option<String> },
    Bookmarked,
    Screenshot { path: String },
    ScreenshotFailed { reason: String },
    RecordingStarted,
    /// A recording whose stream changed goes on in part `part` (2, 3, ...). `cause`: the
    /// engine's "size" | "input" (GPU / CPU frames) | "encoder".
    RecordingContinued { part: u32, cause: String },
    /// A finished part of a recording that goes on: listed, no toast.
    RecordingPartSaved { path: String },
    /// `seconds`: the whole recording, every part; `parts`: the files it took; `gap_seconds`:
    /// time not recorded between the parts; `frozen_seconds`: time the picture held still
    /// (a same-header encoder reopen waiting for its keyframe).
    RecordingStopped { path: String, seconds: f64, parts: u32, gap_seconds: f64, frozen_seconds: f64 },
    /// `files`: what the recording left (it ended early), empty when nothing was saved.
    RecordingFailed { reason: String, files: Vec<String> },
    /// A clip saved to the SSD first reached the clip folder. `kept_path`: the engine kept its
    /// staged copy there for the SSD video cache (`storage::adopt_kept`).
    ClipMoved { path: String, kept_path: Option<String> },
    /// A staged clip could not reach the clip folder yet (it stays safe on the SSD; retried).
    ClipMoveFailed { path: String, reason: String },
    /// Storage limit moved old clips to the Recycle Bin. `through`: last-write Unix seconds of
    /// the newest clip that went ("recordings from <date> and earlier"); bytes after the run.
    StorageCleaned { removed: u64, freed_bytes: u64, through: Option<u64>, used_bytes: u64, cap_bytes: u64 },
    /// Recordings crossed 80 or 90% of the limit, or are over it with nothing left that may go
    /// (100). `next_through`: the clip that would go first.
    StorageWarning { level: u8, used_bytes: u64, cap_bytes: u64, next_through: Option<u64> },
    /// A clip or staging drive is below its free-space threshold (`disk_low`).
    DiskLow { drive: String, free_bytes: u64, need_bytes: u64 },
    /// Dry run of a limit the user picked (answer to `StoragePreview`); nothing moved.
    StoragePreview { cap_gb: u32, used_bytes: u64, removed: u64, freed_bytes: u64, through: Option<u64> },
    Error(String),
}

pub struct DaemonClient {
    shared: Arc<Shared>,
}

/// State shared between the UI-facing client and the connection threads.
struct Shared {
    state: Mutex<protocol::State>,
    revision: AtomicU64,
    /// Line sender of the live connection's writer thread (None while disconnected).
    writer: Mutex<Option<Sender<String>>>,
    stop: AtomicBool,
}

impl Shared {
    fn new() -> Self {
        Self { state: Mutex::new(protocol::State::default()), revision: AtomicU64::new(0), writer: Mutex::new(None), stop: AtomicBool::new(false) }
    }

    fn update(&self, apply: impl FnOnce(&mut protocol::State) -> bool) {
        let changed = match self.state.lock() {
            Ok(mut state) => apply(&mut state),
            Err(poisoned) => apply(&mut poisoned.into_inner()),
        };
        if changed {
            self.revision.fetch_add(1, Ordering::AcqRel);
        }
    }

    fn read<T>(&self, read: impl FnOnce(&mut protocol::State) -> T) -> T {
        match self.state.lock() {
            Ok(mut state) => read(&mut state),
            Err(poisoned) => read(&mut poisoned.into_inner()),
        }
    }

    fn set_writer(&self, writer: Option<Sender<String>>) {
        match self.writer.lock() {
            Ok(mut slot) => *slot = writer,
            Err(poisoned) => *poisoned.into_inner() = writer,
        }
    }
}

impl DaemonClient {
    /// Starts the background connection thread. Never blocks.
    /// `RECALL_CONTROL_PIPE` names another pipe (scripted checks keep off the real recorder).
    pub fn spawn() -> Self {
        let pipe = std::env::var("RECALL_CONTROL_PIPE").ok().filter(|p| !p.is_empty());
        Self::spawn_on(pipe.as_deref().unwrap_or(PIPE_NAME))
    }

    /// `spawn` against any pipe name (tests use a private one).
    fn spawn_on(pipe: &str) -> Self {
        let shared = Arc::new(Shared::new());
        let worker = shared.clone();
        let pipe = pipe.to_owned();
        let spawned = std::thread::Builder::new().name("recall-daemon-pipe".into()).spawn(move || connection_loop(&worker, &pipe));
        if let Err(error) = spawned {
            eprintln!("Recall native: controller pipe thread failed to start: {error}");
        }
        Self { shared }
    }
    /// Queues a command; returns false when not connected (the command is dropped).
    pub fn send(&self, command: DaemonCommand) -> bool {
        let line = protocol::command_line(&command);
        let Ok(writer) = self.shared.writer.lock() else { return false };
        writer.as_ref().is_some_and(|tx| tx.send(line).is_ok())
    }
    /// Increments whenever the snapshot changes or an event arrives.
    pub fn revision(&self) -> u64 {
        self.shared.revision.load(Ordering::Acquire)
    }
    pub fn snapshot(&self) -> DaemonSnapshot {
        self.shared.read(|state| state.snapshot.clone())
    }
    pub fn drain_events(&self) -> Vec<DaemonEvent> {
        self.shared.read(|state| state.events.drain(..).collect())
    }
    /// Connected, but the controller has not answered a status request for [`UNRESPONSIVE`].
    pub fn unresponsive(&self) -> bool {
        self.shared.read(|state| state.unresponsive(std::time::Instant::now(), UNRESPONSIVE))
    }
}

impl Drop for DaemonClient {
    fn drop(&mut self) {
        // The reader exits at its next line or disconnect; the writer when its sender drops.
        self.shared.stop.store(true, Ordering::Release);
        self.shared.set_writer(None);
    }
}

/// Connect, serve until EOF/error, mark disconnected, retry every second.
fn connection_loop(shared: &Shared, pipe: &str) {
    while !shared.stop.load(Ordering::Acquire) {
        if let Ok(name) = pipe.to_ns_name::<GenericNamespaced>()
            && let Ok(stream) = Stream::connect(name)
        {
            serve(shared, stream);
        }
        shared.set_writer(None);
        shared.update(|state| state.set_connected(false));
        std::thread::sleep(RECONNECT);
    }
}

/// One connection: a writer thread drains a channel (UI sends never block), this
/// thread reads lines until EOF or error.
fn serve(shared: &Shared, stream: Stream) {
    let (reader, mut writer) = stream.split();
    let (tx, rx) = mpsc::channel::<String>();
    // Asked once per connect, queued ahead of anything the UI sends.
    let _ = tx.send(protocol::command_line(&DaemonCommand::Status));
    let _ = tx.send(protocol::command_line(&DaemonCommand::GetConfig));
    let spawned = std::thread::Builder::new().name("recall-daemon-pipe-writer".into()).spawn(move || {
        let ping = protocol::command_line(&DaemonCommand::Status);
        loop {
            let line = match rx.recv_timeout(PING) {
                Ok(line) => line,
                Err(mpsc::RecvTimeoutError::Timeout) => ping.clone(),
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
            };
            if writer.write_all(format!("{line}\n").as_bytes()).is_err() {
                break;
            }
        }
    });
    if spawned.is_err() {
        return;
    }
    shared.set_writer(Some(tx));
    shared.update(|state| state.set_connected(true));
    for line in BufReader::new(reader).lines() {
        let Ok(line) = line else { break };
        if shared.stop.load(Ordering::Acquire) {
            break;
        }
        shared.update(|state| state.apply_line(&line));
    }
}

/// Launches the controller (`recall-hdr.exe --record`, hidden, CCD1 affinity inherited)
/// when no controller is running. Ok(false) when one is already running.
pub fn start_controller() -> Result<bool, String> {
    controller::start()
}
