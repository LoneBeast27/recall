//! Capture-engine supervisor.
//!
//! recall-hdr runs the Recall capture engine (`recall.exe --json`) as a
//! supervised child. Supervision is strictly **best-effort**: a missing, broken,
//! or crash-looping engine must never affect display safety, block the
//! display-restore path, or crash the tray. Every failure mode degrades to a
//! logged `GivenUp` health and a quiet manager thread.
//!
//! # Protocol we consume (vendor-parsed, not imported)
//! The engine speaks newline-delimited JSON on stdout: one `{"type":"hello",...}`
//! at startup, then `{"type":"status",...}` lines on change and ~1/s. It accepts
//! `{"cmd":"quit"}\n` on stdin for a clean exit. We parse only the fields we log
//! or cache via `serde_json::Value`. Every line is re-broadcast unchanged to the
//! control pipe (`mod:"engine"`); the last `hello` and `status` are cached so
//! late-joining clients get them ([`Supervisor::cached_engine_events`]).
//!
//! # Targets and restarts
//! [`Supervisor::start`] (legacy) runs one engine that inherits our environment.
//! [`Supervisor::start_idle`] (record mode) runs nothing until
//! [`Supervisor::retarget`] hands it an [`EngineSpec`]; every retarget is an
//! INTENDED restart (quit -> short grace -> kill, then respawn with the new
//! environment) that never counts toward give-up or backoff. `retarget(None)`
//! stops the engine. The restart policy for UNINTENDED exits lives in
//! [`crate::engine_policy`].
//!
//! # Shutdown ordering (assumption)
//! The display-restore path does **not** go through this module. The app layer
//! restores the display first, then calls [`Supervisor::shutdown`], which writes
//! `{"cmd":"quit"}` to the child's stdin, waits up to `grace`, then hard-kills.
//!
//! The manager thread lives in `supervisor_manager.rs`, the stdout protocol
//! reader in `supervisor_protocol.rs`.

#[path = "supervisor_manager.rs"]
mod manager;
#[path = "supervisor_protocol.rs"]
mod protocol;
#[path = "supervisor_stdin.rs"]
mod stdin;

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use crate::engine_policy::EngineSpec;
use manager::run_manager;
pub(crate) use protocol::beep_failure;

/// Windows `CREATE_NO_WINDOW` -- keeps the child from flashing a console window.
const CREATE_NO_WINDOW: u32 = 0x0800_0000;
/// Largest single sleep the manager will take -- keeps shutdown responsive even
/// mid-backoff (a 30s cap sleep is chopped into <=250ms polls of the flag).
const SLEEP_CHUNK: Duration = Duration::from_millis(250);
/// Poll cadence for `try_wait` while supervising / draining a grace period.
const SHUTDOWN_POLL: Duration = Duration::from_millis(50);
/// Grace between `{"cmd":"quit"}` and a hard kill, for retargets and
/// shutdown alike. The engine's own quit path finalizes a long recording
/// (up to 1.5 s) and then lets an in-flight replay save finish (up to 3 s);
/// this outlasts both with room to spare, so a save is never killed.
pub(crate) const QUIT_GRACE: Duration = Duration::from_secs(8);
/// Grace for an intended (retarget) restart before the old engine is killed.
const RETARGET_GRACE: Duration = QUIT_GRACE;

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Health of the supervised capture engine, surfaced to the tray status line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EngineHealth {
    /// The engine is live with this OS process id (0 = spawning).
    Running { pid: u32 },
    /// The engine exited and we are waiting out backoff before attempt `attempt`.
    Restarting { attempt: u32 },
    /// Circuit breaker tripped (too many failures) or the exe couldn't be found.
    /// No further restarts until the next retarget.
    GivenUp,
    /// No engine: shut down, or idle with no target.
    Stopped,
}

/// What the controller derives "recording" from (last engine status line).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct EngineView {
    /// `status.encoder`: "h264_nvenc" | "starting" | "off" | "failed".
    pub encoder: Option<String>,
    pub mic_muted: Option<bool>,
    /// Last engine `status.buffer_secs`: seconds of footage in the replay ring.
    pub buffer_secs: Option<f32>,
    /// Status lines received (all engines): one that stops moving is a hung engine.
    pub statuses: u64,
    /// Last engine `status.encoder_heartbeat` (encoder-thread turns; older engines: none).
    pub heartbeat: Option<u64>,
    /// Last engine `status.encoder_opening`: an NVENC open is in progress.
    pub opening: bool,
}

/// Supervises the capture engine on a background manager thread.
pub struct Supervisor {
    shared: Arc<Shared>,
    handle: Option<JoinHandle<()>>,
}

impl Supervisor {
    /// Legacy: spawn the manager and immediately run an engine that inherits
    /// our environment.
    pub fn start() -> Supervisor {
        Self::start_with_locator(locate_engine, Some(EngineSpec::default()))
    }

    /// Record mode: spawn the manager with no engine until [`retarget`](Self::retarget).
    pub fn start_idle() -> Supervisor {
        Self::start_with_locator(locate_engine, None)
    }

    /// Test seam: injectable engine locator + initial spec.
    fn start_with_locator<F>(locate: F, initial: Option<EngineSpec>) -> Supervisor
    where
        F: FnOnce() -> Option<PathBuf> + Send + 'static,
    {
        let shared = Arc::new(Shared::new(initial));
        let manager_shared = shared.clone();
        let handle = thread::Builder::new()
            .name("recall-engine-supervisor".into())
            .spawn(move || run_manager(manager_shared, locate()))
            .expect("spawn supervisor thread");
        Supervisor { shared, handle: Some(handle) }
    }

    /// Current engine health for the tray status line. Cheap, lock-guarded.
    pub fn health(&self) -> EngineHealth {
        self.shared.health()
    }

    /// Ask the live engine to save its replay buffer. False when it was not queued (no
    /// engine, or one not reading its stdin): the caller says so.
    pub fn send_save_clip(&self) -> bool {
        self.shared.send_line(r#"{"cmd":"save_clip"}"#)
    }

    /// Forward an arbitrary engine protocol command line. False when it was not queued
    /// (no engine, or a full queue: the engine is not reading); never blocks.
    pub fn send_engine_line(&self, line: &str) -> bool {
        self.shared.send_line(line)
    }

    /// Run the engine with `spec` (restarting it if one is live), or stop it
    /// (`None`). Always an intended restart: never a failure, never backoff.
    pub fn retarget(&self, spec: Option<EngineSpec>) {
        self.shared.set_desired(spec);
    }

    /// The last engine `hello` and `status` lines (raw JSON), for late joiners.
    pub fn cached_engine_events(&self) -> Vec<String> {
        let cache = self.shared.cache.lock().unwrap_or_else(|p| p.into_inner());
        cache.hello.iter().chain(cache.status.iter()).cloned().collect()
    }

    pub fn engine_view(&self) -> EngineView {
        self.shared.cache.lock().unwrap_or_else(|p| p.into_inner()).view.clone()
    }

    /// The running engine said it applies `set_source` live (`hello.live_sources`); older
    /// engines ignore that command, so their gain and name changes need a restart.
    pub fn engine_takes_live_sources(&self) -> bool {
        let cache = self.shared.cache.lock().unwrap_or_else(|p| p.into_inner());
        cache.hello.as_deref().and_then(|h| serde_json::from_str::<serde_json::Value>(h).ok()).and_then(|v| v.get("live_sources")?.as_bool()).unwrap_or(false)
    }

    /// Engine `hello`s parsed so far: a change means a fresh engine instance.
    pub fn hello_count(&self) -> u32 {
        self.shared.hello_count.load(Ordering::SeqCst)
    }

    /// Attach the control-pipe hub; engine stdout lines get re-broadcast to it.
    pub fn attach_hub(&self, hub: Arc<crate::control::PipeHub>) {
        if let Ok(mut g) = self.shared.hub.lock() {
            *g = Some(hub);
        }
    }

    /// Also hand the controller the engine events it acts on
    /// ([`FORWARDED_EVENTS`]: saves, long recording, screenshots, bookmarks).
    pub fn attach_events(&self, tx: std::sync::mpsc::Sender<serde_json::Value>) {
        if let Ok(mut g) = self.shared.events.lock() {
            *g = Some(tx);
        }
    }

    /// Clean shutdown: signal the manager to stop, send `{"cmd":"quit"}` to the
    /// child's stdin, wait up to `grace` for a clean exit, then hard-kill any
    /// survivor and join the manager thread.
    pub fn shutdown(mut self, grace: Duration) {
        self.shared.request_stop(grace);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
        self.shared.set_health(EngineHealth::Stopped);
    }
}

impl Drop for Supervisor {
    fn drop(&mut self) {
        // If `shutdown` wasn't called (e.g. a panic upstream), still stop the
        // manager thread so we don't leak a child. Best-effort, short grace.
        if let Some(handle) = self.handle.take() {
            self.shared.request_stop(Duration::from_millis(500));
            let _ = handle.join();
        }
    }
}

// ---------------------------------------------------------------------------
// Shared state between the handle and the manager thread
// ---------------------------------------------------------------------------

#[derive(Default)]
struct EngineCache {
    hello: Option<String>,
    status: Option<String>,
    view: EngineView,
}

struct Shared {
    health: Mutex<EngineHealth>,
    stop: AtomicBool,
    /// Grace period the manager honors once `stop` is set.
    grace_ms: Mutex<u64>,
    /// Grace for intended restarts (tests shorten it).
    restart_grace_ms: AtomicU64,
    /// `(generation, spec)`: every retarget bumps the generation.
    desired: Mutex<(u64, Option<EngineSpec>)>,
    /// Stdin of the live child, so commands / quit can reach it.
    /// The live child's stdin queue (`supervisor_stdin.rs`): held only to queue a line,
    /// never across a write, so neither the pump nor the kill path blocks on a full pipe.
    child_stdin: Mutex<Option<stdin::StdinQueue>>,
    hello_count: AtomicU32,
    spawn_count: AtomicU32,
    cache: Mutex<EngineCache>,
    /// Control-pipe hub: engine protocol lines are re-broadcast to hub clients.
    hub: Mutex<Option<Arc<crate::control::PipeHub>>>,
    /// The controller's engine-event channel (record mode).
    events: Mutex<Option<std::sync::mpsc::Sender<serde_json::Value>>>,
}

/// Engine event types handed to the controller via `attach_events`.
pub const FORWARDED_EVENTS: [&str; 9] = [
    "saved",
    "save_failed",
    "recording_started",
    "recording_stopped",
    "recording_failed",
    "recording_recovered",
    "screenshot",
    "screenshot_failed",
    "bookmarked",
];

impl Shared {
    fn new(initial: Option<EngineSpec>) -> Shared {
        Shared {
            health: Mutex::new(EngineHealth::Stopped),
            stop: AtomicBool::new(false),
            grace_ms: Mutex::new(0),
            restart_grace_ms: AtomicU64::new(RETARGET_GRACE.as_millis() as u64),
            desired: Mutex::new((0, initial)),
            child_stdin: Mutex::new(None),
            hello_count: AtomicU32::new(0),
            spawn_count: AtomicU32::new(0),
            cache: Mutex::new(EngineCache::default()),
            hub: Mutex::new(None),
            events: Mutex::new(None),
        }
    }

    /// Forward one parsed engine event to the controller (if attached).
    fn forward_event(&self, val: &serde_json::Value) {
        let kind = val.get("type").and_then(|t| t.as_str()).unwrap_or("");
        if !FORWARDED_EVENTS.contains(&kind) {
            return;
        }
        if let Ok(g) = self.events.lock() {
            if let Some(tx) = g.as_ref() {
                let _ = tx.send(val.clone());
            }
        }
    }

    /// Re-broadcast one engine protocol line to control-pipe clients (if any).
    /// Non-blocking: the hub queues per client.
    fn hub_engine_event(&self, line: &str) {
        if let Ok(g) = self.hub.lock() {
            if let Some(h) = g.as_ref() {
                h.event("engine", line);
            }
        }
    }

    fn health(&self) -> EngineHealth {
        match self.health.lock() {
            Ok(g) => *g,
            Err(p) => *p.into_inner(),
        }
    }

    fn set_health(&self, h: EngineHealth) {
        match self.health.lock() {
            Ok(mut g) => *g = h,
            Err(p) => *p.into_inner() = h,
        }
    }

    fn request_stop(&self, grace: Duration) {
        if let Ok(mut g) = self.grace_ms.lock() {
            *g = grace.as_millis() as u64;
        }
        self.stop.store(true, Ordering::SeqCst);
    }

    fn stop_requested(&self) -> bool {
        self.stop.load(Ordering::SeqCst)
    }

    fn grace(&self) -> Duration {
        Duration::from_millis(self.grace_ms.lock().map(|g| *g).unwrap_or(0))
    }

    fn restart_grace(&self) -> Duration {
        Duration::from_millis(self.restart_grace_ms.load(Ordering::SeqCst))
    }

    fn desired(&self) -> (u64, Option<EngineSpec>) {
        self.desired.lock().unwrap_or_else(|p| p.into_inner()).clone()
    }

    fn generation(&self) -> u64 {
        self.desired.lock().unwrap_or_else(|p| p.into_inner()).0
    }

    fn set_desired(&self, spec: Option<EngineSpec>) {
        let mut g = self.desired.lock().unwrap_or_else(|p| p.into_inner());
        g.0 += 1;
        g.1 = spec;
    }

    fn set_child_stdin(&self, stdin: Option<std::process::ChildStdin>) {
        self.attach_writer(stdin.map(|s| Box::new(s) as Box<dyn std::io::Write + Send>));
    }

    /// The engine's stdin (tests inject a writer): its own writer thread and queue.
    fn attach_writer(&self, w: Option<Box<dyn std::io::Write + Send>>) {
        let queue = w.map(stdin::StdinQueue::start);
        let mut g = self.child_stdin.lock().unwrap_or_else(|p| p.into_inner());
        *g = queue; // the old queue closes: its writer ends after its current write
    }

    /// Queue one JSON command line for the child, if one runs. Never blocks: a line the
    /// engine is not reading is dropped once its queue is full (`supervisor_stdin.rs`).
    /// True when queued.
    fn send_line(&self, line: &str) -> bool {
        let g = self.child_stdin.lock().unwrap_or_else(|p| p.into_inner());
        g.as_ref().is_some_and(|queue| queue.send(line))
    }

    /// `quit` takes the queue's reserved slot: a healthy engine behind a full queue still
    /// gets its clean finish (`supervisor_stdin.rs`).
    fn send_quit(&self) -> bool {
        let g = self.child_stdin.lock().unwrap_or_else(|p| p.into_inner());
        g.as_ref().is_some_and(|queue| queue.send_quit(r#"{"cmd":"quit"}"#))
    }

    fn clear_cache(&self) {
        *self.cache.lock().unwrap_or_else(|p| p.into_inner()) = EngineCache::default();
    }
}

// ---------------------------------------------------------------------------
// Engine location
// ---------------------------------------------------------------------------

/// Locate the capture-engine exe.
///
/// Search order:
/// 1. **Same directory as our own exe** -- the shipping layout.
/// 2. **Dev fallback**: walk our exe's ancestors for a Cargo `target/` dir and
///    probe `target/debug/recall.exe` then `target/release/recall.exe`.
fn locate_engine() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    locate_sibling_from(&exe, "recall.exe")
}

/// Core of the sibling lookup (engine + overlay), unit-testable against a
/// synthetic exe path under a temp dir.
pub(crate) fn locate_sibling_from(current_exe: &Path, file: &str) -> Option<PathBuf> {
    if let Some(dir) = current_exe.parent() {
        let sibling = dir.join(file);
        if sibling.is_file() {
            return Some(sibling);
        }
    }
    for ancestor in current_exe.ancestors() {
        if ancestor.file_name().map(|n| n == "target").unwrap_or(false) {
            for profile in ["debug", "release"] {
                let candidate = ancestor.join(profile).join(file);
                if candidate.is_file() {
                    return Some(candidate);
                }
            }
        }
    }
    None
}

fn locate_engine_from(current_exe: &Path) -> Option<PathBuf> {
    locate_sibling_from(current_exe, "recall.exe")
}

/// Build `recall.exe --json` with piped stdio, no console, and the spec's
/// environment edits.
fn engine_command(path: &Path, spec: &EngineSpec) -> Command {
    use std::os::windows::process::CommandExt;
    let mut cmd = Command::new(path);
    cmd.arg("--json")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .creation_flags(CREATE_NO_WINDOW);
    for (key, value) in &spec.env {
        match value {
            Some(v) => cmd.env(key, v),
            None => cmd.env_remove(key),
        };
    }
    cmd
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

/// An engine stdin for tests elsewhere: `send` answers as `Supervisor::send_engine_line`.
#[cfg(test)]
pub(crate) struct TestPipe(Arc<Shared>);

#[cfg(test)]
impl TestPipe {
    /// A pipe whose engine reads nothing (every write blocks) and whose queue is full. The
    /// fill starts only once the writer has taken its line and parked, so no slot can free
    /// up afterwards (`tests::wedge_and_fill`).
    pub fn wedged_and_full() -> TestPipe {
        let shared = Arc::new(Shared::new(None));
        tests::wedge_and_fill(&shared);
        TestPipe(shared)
    }

    /// A pipe whose engine reads every line.
    pub fn healthy() -> TestPipe {
        let shared = Arc::new(Shared::new(None));
        shared.attach_writer(Some(Box::new(std::io::sink())));
        TestPipe(shared)
    }

    pub fn send(&self, line: &str) -> bool {
        self.0.send_line(line)
    }
}

#[cfg(test)]
#[path = "supervisor_tests.rs"]
mod tests;
