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

#[path = "supervisor_hello.rs"]
mod hello;
#[path = "supervisor_manager.rs"]
mod manager;
#[path = "supervisor_mute.rs"]
mod mute;
#[path = "supervisor_protocol.rs"]
mod protocol;
#[path = "supervisor_refusal.rs"]
mod refusal;
#[path = "supervisor_release.rs"]
mod release;
#[path = "supervisor_stdin.rs"]
mod stdin;
#[path = "supervisor_locate.rs"]
mod locate;

use std::path::PathBuf;
#[cfg(test)]
use std::{path::Path, process::Command};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use crate::engine_policy::EngineSpec;
use crate::record_dispatch::Sent;
use locate::{engine_command, locate_engine};
#[cfg(test)]
use locate::locate_engine_from;
pub(crate) use locate::locate_sibling_from;
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

    /// Whether another recorder holds the engine slot (busy retries, or given up on it).
    pub fn slot_held(&self) -> crate::engine_slot::SlotHeld {
        self.shared.slot.get(self.shared.hello_count.load(Ordering::SeqCst))
    }

    /// Forward an arbitrary engine protocol command line; never blocks. Not queued (no
    /// engine, a full queue, or an engine told to quit) says why ([`Sent`]).
    pub fn send_engine_line(&self, line: &str) -> Sent {
        self.shared.send_line(line)
    }

    /// Run the engine with `spec` (restarting it if one is live), or stop it
    /// (`None`), for a game switch. Always an intended restart: never a failure, never
    /// backoff. Lines sent from the old engine's `quit` until the next engine is up
    /// are refused as [`Sent::Switching`]; for a stop, [`Sent::Waiting`] (no game follows).
    pub fn retarget(&self, spec: Option<EngineSpec>) {
        self.shared.set_desired(spec, Sent::Switching);
    }

    /// As [`retarget`](Self::retarget), for a restart that is no game switch. `planned`
    /// (new settings, a resume): lines sent meanwhile are refused as [`Sent::Restarting`];
    /// otherwise (a hung or failed engine, the capture watchdog) as [`Sent::NotResponding`].
    pub fn restart(&self, spec: Option<EngineSpec>, planned: bool) {
        self.shared.set_desired(spec, if planned { Sent::Restarting } else { Sent::NotResponding });
    }

    /// Stop the engine for a pause: lines sent while it quits are refused as [`Sent::Paused`].
    pub fn pause(&self) {
        self.shared.set_desired(None, Sent::Paused);
    }

    /// A resume: lines sent while the paused engine still quits say "waiting for a game"
    /// from now (a game taking the engine says "Switching games" through `retarget`).
    pub fn unpause(&self) {
        self.shared.unpause();
    }

    /// Alt+M: the mic mute every later spawn starts with, a crash respawn included, and the
    /// `set_mic_mute` line to the live engine, as one step (`supervisor_mute.rs`).
    pub fn set_mic_muted(&self, muted: bool) -> Sent {
        self.shared.set_mute_and_send(muted)
    }

    /// The last engine `hello` and `status` lines (raw JSON), for late joiners.
    pub fn cached_engine_events(&self) -> Vec<String> {
        let cache = self.shared.cache.lock().unwrap_or_else(|p| p.into_inner());
        cache.hello.iter().chain(cache.status.iter()).cloned().collect()
    }

    pub fn engine_view(&self) -> EngineView {
        self.shared.cache.lock().unwrap_or_else(|p| p.into_inner()).view.clone()
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

    /// Stop re-broadcasting engine lines (a quit that gave up on a save reports it itself).
    pub fn detach_hub(&self) {
        if let Ok(mut g) = self.shared.hub.lock() {
            *g = None;
        }
    }

    /// Also hand the controller the engine events it acts on
    /// ([`FORWARDED_EVENTS`]: saves and the save writing, long recording, screenshots, bookmarks).
    pub fn attach_events(&self, tx: std::sync::mpsc::Sender<serde_json::Value>) {
        if let Ok(mut g) = self.shared.events.lock() {
            *g = Some(tx);
        }
    }

    /// Clean shutdown: signal the manager to stop, send `{"cmd":"quit"}` to the
    /// child's stdin, wait up to `grace` for a clean exit, then hard-kill any
    /// survivor and join the manager thread.
    pub fn shutdown(mut self, grace: Duration) {
        self.stop(grace);
    }

    /// `shutdown` in place (a session end runs it from a window handler). Safe to call again.
    pub fn stop(&mut self, grace: Duration) {
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
    /// `(generation, spec, refusal while the old engine quits)`: every retarget bumps the generation.
    desired: Mutex<(u64, Option<EngineSpec>, Sent)>,
    /// The generation of the last release (`supervisor_release.rs`); 0 = none.
    release_gen: AtomicU64,
    /// The release whose saves the controller saw land (`Supervisor::release_settled`).
    settled_gen: AtomicU64,
    /// The live child's stdin queue (`supervisor_stdin.rs`): held only to queue a line,
    /// never across a write, so neither the pump nor the kill path blocks on a full pipe.
    child_stdin: Mutex<EngineStdin>,
    hello_count: AtomicU32,
    spawn_count: AtomicU32,
    /// The mic mute each spawn starts with, over the spec's (`supervisor_mute.rs`).
    spawn_mute: mute::SpawnMute,
    cache: Mutex<EngineCache>,
    /// Control-pipe hub: engine protocol lines are re-broadcast to hub clients.
    hub: Mutex<Option<Arc<crate::control::PipeHub>>>,
    /// The controller's engine-event channel (record mode).
    events: Mutex<Option<std::sync::mpsc::Sender<serde_json::Value>>>,
    /// Another recorder holds the engine slot (`engine_slot`).
    slot: crate::engine_slot::SlotFlag,
}

/// The live child's stdin and whether it was told to quit, under one lock: the check and
/// the queueing of a line are one step, so no line can land behind the `quit` (the engine
/// stops reading there and would drop it unanswered: third audit U1).
#[derive(Default)]
struct EngineStdin {
    queue: Option<stdin::StdinQueue>,
    /// Set with the `quit`: why every line is refused until the next engine's stdin.
    quitting: Option<Sent>,
}

/// The event the manager hands the controller each time an engine process
/// ends (any reason): saves still in flight are lost (`EngineActivity`).
pub const ENGINE_EXITED: &str = "engine_exited";

/// Engine event types handed to the controller via `attach_events`. `status`
/// carries `save_writing`, the save the engine is still writing.
pub const FORWARDED_EVENTS: [&str; 12] = [
    "status",
    "save_progress",
    "saved",
    "source_adopted",
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
            desired: Mutex::new((0, initial, Sent::NotResponding)),
            release_gen: AtomicU64::new(0),
            settled_gen: AtomicU64::new(0),
            child_stdin: Mutex::new(EngineStdin::default()),
            hello_count: AtomicU32::new(0),
            spawn_count: AtomicU32::new(0),
            spawn_mute: mute::SpawnMute::new(),
            cache: Mutex::new(EngineCache::default()),
            hub: Mutex::new(None),
            events: Mutex::new(None),
            slot: Default::default(),
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

    /// Tell the controller an engine process ended ([`ENGINE_EXITED`]); sent
    /// after its stdout reader is joined, so after every event it wrote.
    fn forward_exit(&self) {
        if let Ok(g) = self.events.lock() {
            if let Some(tx) = g.as_ref() {
                let _ = tx.send(serde_json::json!({ "type": ENGINE_EXITED }));
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
        self.shutdown_refusal(); // a retarget's grace in flight: "isn't responding" from now
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
        let g = self.desired.lock().unwrap_or_else(|p| p.into_inner());
        (g.0, g.1.clone())
    }

    fn generation(&self) -> u64 {
        self.desired.lock().unwrap_or_else(|p| p.into_inner()).0
    }

    /// A spawned engine's stdin; `baked` is the mute its environment was given.
    fn set_child_stdin(&self, stdin: Option<std::process::ChildStdin>, baked: Option<bool>) {
        self.attach_spawned(stdin.map(|s| Box::new(s) as Box<dyn std::io::Write + Send>), baked);
    }

    /// The engine's stdin (tests inject a writer): its own writer thread and queue.
    /// No mute check: the baked mute is taken as the current one.
    fn attach_writer(&self, w: Option<Box<dyn std::io::Write + Send>>) {
        self.attach_spawned(w, self.spawn_mute.get());
    }

    /// Queue one JSON command line for the child, if one runs and was not told to quit.
    /// Never blocks: a line the engine is not reading is dropped once its queue is full
    /// (`supervisor_stdin.rs`).
    fn send_line(&self, line: &str) -> Sent {
        let g = self.child_stdin.lock().unwrap_or_else(|p| p.into_inner());
        self.queue_line(&g, line)
    }

    /// [`send_line`](Self::send_line) under a `child_stdin` lock the caller holds.
    fn queue_line(&self, g: &EngineStdin, line: &str) -> Sent {
        if let Some(refusal) = g.quitting {
            return refusal; // the engine was told to quit: it would never read this
        }
        if g.queue.as_ref().is_some_and(|queue| queue.send(line)) { Sent::Queued } else { Sent::NotResponding }
    }

    fn clear_cache(&self) {
        *self.cache.lock().unwrap_or_else(|p| p.into_inner()) = EngineCache::default();
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

/// An engine stdin for tests elsewhere (`supervisor_quit_tests.rs`).
#[cfg(test)]
pub(crate) use quit_tests::TestPipe;

#[cfg(test)]
#[path = "supervisor_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "supervisor_quit_tests.rs"]
pub(crate) mod quit_tests;

#[cfg(test)]
#[path = "supervisor_stop_tests.rs"]
mod stop_tests;

#[cfg(test)]
#[path = "supervisor_refusal_tests.rs"]
mod refusal_tests;
