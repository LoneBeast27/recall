//! Client for the controller's control pipe (`\\.\pipe\recall-hdr-control-<session>`, `session`).
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
use serde_json::Value;

mod controller;
mod pipe_owner;
mod protocol;
mod reach;
mod testing;
mod session;
mod status;

// The examples include this file without the views that use these.
#[allow(unused_imports)]
pub use reach::{reach, reach_or_refused, unreachable_detail, RecorderReach};
pub use status::*;
#[cfg(test)]
mod tests;
#[cfg(test)]
mod tests_pipe;
#[cfg(test)]
mod tests_clock;

const RECONNECT: Duration = Duration::from_secs(1);
/// A `status` request this often: only the controller's pump answers it, so the
/// replies show it is alive (engine lines come from another thread).
const PING: Duration = Duration::from_secs(2);
/// No `session` status for this long on an open pipe = the recorder is not responding.
pub const UNRESPONSIVE: Duration = Duration::from_secs(5);

#[derive(Debug, Clone, PartialEq)]
pub enum DaemonCommand {
    Status,
    /// Stop the recorder; it finishes a clip being saved first (a repeat joins that wait).
    Quit,
    /// Stop now, even while a clip is being saved (the clip is lost): `"force":true`.
    QuitNow,
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
    /// Settings → Games, one app at a time: `how` = "record" | "never" | "fullscreen" |
    /// "remove" | "unlist". The controller applies it to its current config, so two quick
    /// edits (or a tray "Add as a game" in between) never overwrite each other (R8-detect-7).
    RegisterGame { exe: String, how: &'static str },
    /// What `storage.cap_gb` = this would move to the Recycle Bin now (answered by a
    /// `StoragePreview` event; nothing moves).
    StoragePreview(u32),
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
    /// (a same-header encoder reopen waiting for its keyframe); `locked_seconds`: time the PC
    /// sat locked (Win+L) while it ran.
    RecordingStopped { path: String, seconds: f64, parts: u32, gap_seconds: f64, frozen_seconds: f64, locked_seconds: f64 },
    /// The PC unlocked after a lock during which a long recording kept going (`seconds` locked).
    RecordingKeptLocked { seconds: f64 },
    /// The PC unlocked after a lock during which a long recording ran and then stopped.
    RecordingStoppedLocked { seconds: f64 },
    /// The PC unlocked; a long recording ran for part of the lock (it stopped and started
    /// again, or started during it) and is running now.
    RecordingPartlyLocked { seconds: f64 },
    /// A sign-out / shutdown another app cancelled: the long recording it stopped goes on in
    /// a new file.
    ShutdownCancelled,
    /// `files`: what the recording left (it ended early), empty when nothing was saved.
    RecordingFailed { reason: String, files: Vec<String>, gap_seconds: f64, frozen_seconds: f64 },
    /// Long recordings a crash or a kill left as `.part`, renamed into place on the next
    /// engine start (`paths`, one event for all); gap / frozen seconds when known (else 0).
    RecordingRecovered { paths: Vec<String>, gap_seconds: f64, frozen_seconds: f64 },
    /// "Sort clips into game folders" found the user's own folder with the game's name in
    /// Recordings, not answered yet (the clip went straight into Recordings): ask "Use your
    /// existing <Game> folder?" (`views::game_folder_prompt`).
    GameFolderExists { game: String, folder: String },
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
    /// The recorder repaired `count` invalid settings when it loaded config.toml (once per load).
    ConfigRepaired { count: u64 },
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
        if crate::TESTING {
            return Self::testing();
        }
        let pipe = std::env::var("RECALL_CONTROL_PIPE").ok().filter(|p| !p.is_empty());
        // The real recorder's pipe is per Windows session (`session`).
        Self::spawn_on(&pipe.unwrap_or_else(session::current_pipe_name))
    }

    /// `spawn` against any pipe name (tests use a private one).
    fn spawn_on(pipe: &str) -> Self {
        Self::spawn_checked(pipe, pipe_owner::check)
    }

    /// `spawn_on` with the server-owner check injected (tests fake another account).
    fn spawn_checked(pipe: &str, owner: OwnerCheck) -> Self {
        let shared = Arc::new(Shared::new());
        let worker = shared.clone();
        let pipe = pipe.to_owned();
        let spawned = std::thread::Builder::new().name("recall-daemon-pipe".into()).spawn(move || connection_loop(&worker, &pipe, owner));
        if let Err(error) = spawned {
            eprintln!("Recall native: controller pipe thread failed to start: {error}");
        }
        Self { shared }
    }
    /// Queues a command; returns false when not connected (the command is dropped).
    pub fn send(&self, command: DaemonCommand) -> bool {
        if crate::TESTING {
            return self.send_testing(command);
        }
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

/// Whether a fresh connection's server may be used (`pipe_owner::check`).
type OwnerCheck = fn(&Stream) -> Result<(), String>;

/// Connect, check the server runs as this account, serve until EOF/error, mark
/// disconnected, retry every second. A refused server gets nothing written to it.
fn connection_loop(shared: &Shared, pipe: &str, owner: OwnerCheck) {
    while !shared.stop.load(Ordering::Acquire) {
        if let Ok(name) = pipe.to_ns_name::<GenericNamespaced>()
            && let Ok(stream) = Stream::connect(name)
        {
            let verdict = owner(&stream);
            let refused = verdict.as_ref().err().cloned();
            if let Some(reason) = &refused
                && shared.read(|state| state.snapshot.refused.as_ref() != Some(reason))
            {
                eprintln!("Recall native: {reason}; not using the control pipe");
            }
            shared.update(|state| state.set_refused(refused));
            if verdict.is_ok() {
                serve(shared, stream);
            }
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

/// Connect the setup runner to a paused recorder without starting capture.
pub(crate) fn start_controller_for_setup() -> Result<bool, String> {
    controller::start_for_setup()
}

/// A recorder process runs in this Windows session (false when the list can't be read).
pub fn controller_running() -> bool {
    controller::process_running(controller::CONTROLLER_EXE).unwrap_or(false)
}
