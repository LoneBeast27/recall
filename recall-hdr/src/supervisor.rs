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

use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use crate::engine_policy::{EngineSpec, Next, RestartPolicy, GIVEUP_WINDOW};
use crate::log;

/// Windows `CREATE_NO_WINDOW` -- keeps the child from flashing a console window.
const CREATE_NO_WINDOW: u32 = 0x0800_0000;
/// Largest single sleep the manager will take -- keeps shutdown responsive even
/// mid-backoff (a 30s cap sleep is chopped into <=250ms polls of the flag).
const SLEEP_CHUNK: Duration = Duration::from_millis(250);
/// Poll cadence for `try_wait` while supervising / draining a grace period.
const SHUTDOWN_POLL: Duration = Duration::from_millis(50);
/// Grace for an intended (retarget) restart before the old engine is killed.
const RETARGET_GRACE: Duration = Duration::from_secs(3);

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
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EngineView {
    /// `status.encoder`: "h264_nvenc" | "starting" | "off" | "failed".
    pub encoder: Option<String>,
    pub mic_muted: Option<bool>,
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

    /// Ask the live engine to save its replay buffer. Best-effort: a no-op when
    /// no engine is running (Saved / SaveFailed events are the visible outcome).
    pub fn send_save_clip(&self) {
        self.shared.send_line(r#"{"cmd":"save_clip"}"#);
    }

    /// Forward an arbitrary engine protocol command line (control-pipe path).
    pub fn send_engine_line(&self, line: &str) {
        self.shared.send_line(line);
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
    child_stdin: Mutex<Option<std::process::ChildStdin>>,
    hello_count: AtomicU32,
    spawn_count: AtomicU32,
    cache: Mutex<EngineCache>,
    /// Control-pipe hub: engine protocol lines are re-broadcast to hub clients.
    hub: Mutex<Option<Arc<crate::control::PipeHub>>>,
}

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
        if let Ok(mut g) = self.child_stdin.lock() {
            *g = stdin;
        }
    }

    /// Send one JSON command line to the child if we hold its stdin. Best-effort.
    fn send_line(&self, line: &str) {
        if let Ok(mut g) = self.child_stdin.lock() {
            if let Some(stdin) = g.as_mut() {
                let _ = stdin.write_all(line.as_bytes());
                let _ = stdin.write_all(b"\n");
                let _ = stdin.flush();
            }
        }
    }

    fn send_quit(&self) {
        self.send_line(r#"{"cmd":"quit"}"#);
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

// ---------------------------------------------------------------------------
// Manager thread
// ---------------------------------------------------------------------------

/// Why a supervised child stopped.
enum ChildExit {
    Clean(i32),
    Signalled,
    /// Shutdown was requested; the caller drives quit -> grace -> kill.
    StopRequested,
    /// A retarget arrived; the caller restarts on purpose.
    Retarget,
}

/// The supervision loop. Owns the child across its whole lifetime and never
/// panics out -- every failure path logs and either backs off or parks.
fn run_manager(shared: Arc<Shared>, engine_path: Option<PathBuf>) {
    let Some(path) = engine_path else {
        log!("supervisor: capture engine (recall.exe) not found next to the tray exe or under target/{{debug,release}}; running without a supervised engine.");
        shared.set_health(EngineHealth::GivenUp);
        return;
    };
    log!("supervisor: capture engine at {}", path.display());

    let mut policy = RestartPolicy::default();
    let mut seen_gen = shared.generation();
    loop {
        if shared.stop_requested() {
            break;
        }
        let (gen, spec) = shared.desired();
        if gen != seen_gen {
            policy.reset(); // a new target gets a fresh circuit breaker
            seen_gen = gen;
        }
        let Some(spec) = spec else {
            shared.set_health(EngineHealth::Stopped);
            if !wait_for_change(&shared, gen) {
                break;
            }
            continue;
        };

        shared.set_health(EngineHealth::Running { pid: 0 });
        let started = Instant::now();
        match spawn_engine(&path, &shared, &spec) {
            Ok((mut child, reader)) => {
                let pid = child.id();
                shared.set_health(EngineHealth::Running { pid });
                log!("supervisor: engine started (pid {pid})");
                let exit = supervise_child(&shared, &mut child, gen);
                let grace = match exit {
                    ChildExit::StopRequested => Some(shared.grace()),
                    ChildExit::Retarget => Some(shared.restart_grace()),
                    _ => None,
                };
                if let Some(grace) = grace {
                    // Run quit->grace->kill FIRST, while we still hold stdin;
                    // only a dead child closes stdout and unblocks the reader.
                    finish_shutdown(&shared, child, grace);
                }
                shared.set_child_stdin(None);
                let _ = reader.join();
                shared.clear_cache();
                match exit {
                    ChildExit::StopRequested => break,
                    ChildExit::Retarget => {
                        log!("supervisor: engine retargeted (intended restart; not a failure)");
                        policy.on_intended_restart();
                        continue;
                    }
                    ChildExit::Clean(code) => log!("supervisor: engine exited (code {code})"),
                    ChildExit::Signalled => log!("supervisor: engine terminated without an exit code"),
                }
            }
            Err(e) => log!("supervisor: failed to spawn engine: {e}"),
        }

        match policy.on_unintended_exit(Instant::now(), started.elapsed()) {
            Next::GiveUp => {
                log!(
                    "supervisor: giving up -- {} engine failures within {}s; not restarting until the target changes.",
                    policy.recent_failures(),
                    GIVEUP_WINDOW.as_secs()
                );
                shared.set_health(EngineHealth::GivenUp);
                if !wait_for_change(&shared, gen) {
                    break;
                }
            }
            Next::Backoff { delay, attempt } => {
                shared.set_health(EngineHealth::Restarting { attempt });
                log!("supervisor: restarting engine in {}ms (attempt {attempt})", delay.as_millis());
                if !sleep_unless_change(&shared, delay, gen) {
                    break;
                }
            }
        }
    }

    shared.set_health(EngineHealth::Stopped);
    log!("supervisor: manager thread stopped");
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

/// Spawn the engine. Hands the child's stdin to `shared` and spawns the stdout
/// reader thread. Returns the child and the reader's join handle.
fn spawn_engine(path: &Path, shared: &Arc<Shared>, spec: &EngineSpec) -> std::io::Result<(Child, JoinHandle<()>)> {
    let mut child = engine_command(path, spec).spawn()?;
    shared.spawn_count.fetch_add(1, Ordering::SeqCst);
    shared.set_child_stdin(child.stdin.take());

    let stdout = child.stdout.take();
    let stderr = child.stderr.take();
    let reader_shared = shared.clone();
    let reader = thread::Builder::new()
        .name("recall-engine-reader".into())
        .spawn(move || {
            let stderr_join = stderr.map(|se| {
                thread::Builder::new()
                    .name("recall-engine-stderr".into())
                    .spawn(move || drain_stderr(se))
                    .expect("spawn stderr reader")
            });
            if let Some(so) = stdout {
                read_protocol(&reader_shared, so);
            }
            if let Some(j) = stderr_join {
                let _ = j.join();
            }
        })
        .expect("spawn reader thread");
    Ok((child, reader))
}

/// Block until the child exits, shutdown is requested, or a retarget arrives.
fn supervise_child(shared: &Arc<Shared>, child: &mut Child, gen: u64) -> ChildExit {
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                return match status.code() {
                    Some(code) => ChildExit::Clean(code),
                    None => ChildExit::Signalled,
                };
            }
            Ok(None) => {
                if shared.stop_requested() {
                    return ChildExit::StopRequested;
                }
                if shared.generation() != gen {
                    return ChildExit::Retarget;
                }
                thread::sleep(SHUTDOWN_POLL);
            }
            Err(_) => return ChildExit::Signalled,
        }
    }
}

/// Ordered stop of a still-running child: send `quit`, wait up to `grace`
/// polling `try_wait`, then hard-kill any survivor.
fn finish_shutdown(shared: &Arc<Shared>, mut child: Child, grace: Duration) {
    let pid = child.id();
    if let Ok(Some(_)) = child.try_wait() {
        log!("supervisor: engine (pid {pid}) already exited before shutdown");
        return;
    }
    log!("supervisor: stopping engine (pid {pid}); grace {}ms", grace.as_millis());
    shared.send_quit();
    let deadline = Instant::now() + grace;
    while Instant::now() < deadline {
        match child.try_wait() {
            Ok(Some(status)) => {
                log!("supervisor: engine exited cleanly on quit (code {})", status.code().unwrap_or(-1));
                return;
            }
            Ok(None) => thread::sleep(SHUTDOWN_POLL),
            Err(_) => break,
        }
    }
    // Grace expired -- hard kill and reap so no orphan is left holding the capture.
    log!("supervisor: grace expired; killing engine (pid {pid})");
    let _ = child.kill();
    let _ = child.wait();
}

/// Park until the desired generation changes (`true`) or stop (`false`).
fn wait_for_change(shared: &Arc<Shared>, gen: u64) -> bool {
    while !shared.stop_requested() {
        if shared.generation() != gen {
            return true;
        }
        thread::sleep(SHUTDOWN_POLL);
    }
    false
}

/// Sleep `total` in short slices. `false` = stop requested; returns `true`
/// early when a retarget arrives (it must not wait out a crash backoff).
fn sleep_unless_change(shared: &Arc<Shared>, total: Duration, gen: u64) -> bool {
    let deadline = Instant::now() + total;
    while Instant::now() < deadline {
        if shared.stop_requested() {
            return false;
        }
        if shared.generation() != gen {
            return true;
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        thread::sleep(remaining.min(SLEEP_CHUNK).min(SHUTDOWN_POLL));
    }
    !shared.stop_requested()
}

// ---------------------------------------------------------------------------
// Protocol reader (vendor-parsed -- we do NOT import the `recall` crate)
// ---------------------------------------------------------------------------

/// Compact digest of a status line; we only re-log when it changes.
#[derive(PartialEq, Eq, Clone, Default)]
struct StatusDigest {
    display: String,
    mode: String,
    source: String,
}

/// Read newline-delimited JSON from the engine's stdout: relay every line,
/// cache hello/status, log the interesting transitions.
fn read_protocol(shared: &Arc<Shared>, stdout: std::process::ChildStdout) {
    let mut last: Option<StatusDigest> = None;
    for line in BufReader::new(stdout).lines() {
        let Ok(line) = line else { break };
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let Ok(val) = serde_json::from_str::<serde_json::Value>(line) else {
            continue; // ignore non-JSON / malformed lines
        };
        shared.hub_engine_event(line);
        match val.get("type").and_then(|t| t.as_str()) {
            Some("hello") => {
                let version = val.get("version").and_then(|v| v.as_str()).unwrap_or("?");
                let monitors = val.get("monitors").and_then(|m| m.as_array()).map(|a| a.len()).unwrap_or(0);
                log!("supervisor: engine hello -- version {version}, {monitors} monitor(s)");
                shared.cache.lock().unwrap_or_else(|p| p.into_inner()).hello = Some(line.to_string());
                shared.hello_count.fetch_add(1, Ordering::SeqCst);
            }
            Some("status") => {
                {
                    let mut cache = shared.cache.lock().unwrap_or_else(|p| p.into_inner());
                    cache.status = Some(line.to_string());
                    cache.view.encoder = val.get("encoder").and_then(|v| v.as_str()).map(str::to_owned);
                    if let Some(m) = val.get("mic_muted").and_then(|v| v.as_bool()) {
                        cache.view.mic_muted = Some(m);
                    }
                }
                let digest = StatusDigest {
                    display: field(&val, "display"),
                    mode: field(&val, "mode"),
                    source: field(&val, "source"),
                };
                // Log only on a real transition -- not every ~1/s fps tick.
                if last.as_ref() != Some(&digest) {
                    log!(
                        "supervisor: engine status -- display={} mode={} source={:?}",
                        digest.display,
                        digest.mode,
                        digest.source
                    );
                    last = Some(digest);
                }
            }
            Some("mic_muted") => {
                if let Some(m) = val.get("muted").and_then(|v| v.as_bool()) {
                    shared.cache.lock().unwrap_or_else(|p| p.into_inner()).view.mic_muted = Some(m);
                }
            }
            Some("saved") => {
                let path = field(&val, "path");
                let secs = val.get("seconds").and_then(|v| v.as_f64()).unwrap_or(0.0);
                let ms = val.get("elapsed_ms").and_then(|v| v.as_u64()).unwrap_or(0);
                log!("supervisor: clip SAVED ({secs:.1}s, {ms}ms) -> {path}");
                beep(0x00); // MB_OK -- R10 instant success feedback
            }
            Some("save_failed") => {
                log!("supervisor: clip save FAILED -- {}", field(&val, "reason"));
                beep_failure();
            }
            Some("encoder_log") => {
                log!("supervisor: engine encoder -- {}", field(&val, "message"));
            }
            _ => {} // unknown message types are ignored
        }
    }
}

/// user32 MessageBeep, linked directly (windows-rs keeps relocating it across
/// releases; the raw import is version-proof). Async, non-blocking.
fn beep(utype: u32) {
    #[link(name = "user32")]
    extern "system" {
        fn MessageBeep(utype: u32) -> i32;
    }
    unsafe {
        let _ = MessageBeep(utype);
    }
}

/// The save-failed sound (MB_ICONHAND), also used when no engine can save.
pub(crate) fn beep_failure() {
    beep(0x10);
}

/// Pull a string field from a JSON object, defaulting to empty.
fn field(val: &serde_json::Value, key: &str) -> String {
    val.get(key).and_then(|v| v.as_str()).unwrap_or("").to_string()
}

/// Log the engine's stderr, line by line, so a broken engine is diagnosable.
fn drain_stderr(stderr: std::process::ChildStderr) {
    for line in BufReader::new(stderr).lines() {
        let Ok(line) = line else { break };
        let line = line.trim();
        if !line.is_empty() {
            log!("supervisor: engine-stderr: {line}");
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    /// A unique scratch dir under the OS temp dir.
    fn temp_dir(tag: &str) -> PathBuf {
        let mut p = std::env::temp_dir();
        p.push(format!(
            "recall-hdr-suptest-{tag}-{}-{:?}",
            std::process::id(),
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()
        ));
        fs::create_dir_all(&p).unwrap();
        p
    }

    fn touch(path: &Path) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(path, b"").unwrap();
    }

    fn wait_for(deadline_secs: u64, mut f: impl FnMut() -> bool) -> bool {
        let deadline = Instant::now() + Duration::from_secs(deadline_secs);
        while Instant::now() < deadline {
            if f() {
                return true;
            }
            thread::sleep(Duration::from_millis(20));
        }
        f()
    }

    // --- exe-location fallback order --------------------------------------

    #[test]
    fn locate_prefers_sibling_of_current_exe() {
        let root = temp_dir("sibling");
        let install = root.join("install");
        let our_exe = install.join("recall-hdr.exe");
        let engine = install.join("recall.exe");
        touch(&our_exe);
        touch(&engine);
        touch(&root.join("target").join("debug").join("recall.exe"));
        let found = locate_engine_from(&our_exe).expect("engine located");
        assert_eq!(found, engine, "sibling install dir must win over target/");
    }

    #[test]
    fn locate_dev_fallback_prefers_debug_over_release() {
        let root = temp_dir("devfallback");
        let our_exe = root.join("target").join("debug").join("recall-hdr.exe");
        touch(&our_exe);
        let debug_engine = root.join("target").join("debug").join("recall.exe");
        let release_engine = root.join("target").join("release").join("recall.exe");
        touch(&debug_engine);
        touch(&release_engine);
        let found = locate_engine_from(&our_exe).expect("engine located");
        assert_eq!(found, debug_engine, "debug engine (sibling) found first");
    }

    #[test]
    fn locate_dev_fallback_release_when_no_debug() {
        let root = temp_dir("releaseonly");
        let our_exe = root.join("target").join("debug").join("deps").join("recall-hdr.exe");
        touch(&our_exe);
        let release_engine = root.join("target").join("release").join("recall.exe");
        touch(&release_engine);
        let found = locate_engine_from(&our_exe).expect("engine located via fallback");
        assert_eq!(found, release_engine, "falls back to release when no debug");
    }

    #[test]
    fn locate_returns_none_when_absent() {
        let root = temp_dir("absent");
        let our_exe = root.join("bin").join("recall-hdr.exe");
        touch(&our_exe);
        assert!(locate_engine_from(&our_exe).is_none());
    }

    // --- missing engine gives up without panicking ------------------------

    #[test]
    fn missing_engine_gives_up_cleanly() {
        let sup = Supervisor::start_with_locator(|| None, Some(EngineSpec::default()));
        assert!(wait_for(2, || sup.health() == EngineHealth::GivenUp), "missing engine -> GivenUp");
        sup.shutdown(Duration::from_millis(200));
    }

    // --- spawn environment --------------------------------------------------

    #[test]
    fn engine_command_sets_and_removes_spec_env() {
        let spec = EngineSpec {
            env: vec![
                ("RECALL_CAPTURE_NAME".into(), Some("VALORANT".into())),
                ("RECALL_CAPTURE_HWND".into(), None),
            ],
        };
        let cmd = engine_command(Path::new("recall.exe"), &spec);
        let envs: Vec<_> = cmd.get_envs().map(|(k, v)| (k.to_owned(), v.map(|v| v.to_owned()))).collect();
        assert!(envs.contains(&("RECALL_CAPTURE_NAME".into(), Some("VALORANT".into()))));
        assert!(envs.contains(&("RECALL_CAPTURE_HWND".into(), None)), "None removes an inherited value");
        assert_eq!(cmd.get_args().collect::<Vec<_>>(), vec!["--json"]);
    }

    // --- intended restarts (fake engine: cmd.exe idles on its piped stdin) --

    /// `cmd.exe` stands in for the engine: it ignores `--json`, idles reading
    /// stdin, and never answers `quit`, so every stop exercises grace -> kill.
    /// No capture, no GPU.
    fn fake_engine() -> PathBuf {
        let comspec = std::env::var_os("ComSpec").map(PathBuf::from);
        comspec.unwrap_or_else(|| PathBuf::from(r"C:\Windows\System32\cmd.exe"))
    }

    #[test]
    fn retargets_restart_on_purpose_and_never_trip_give_up() {
        let engine = fake_engine();
        let sup = Supervisor::start_with_locator(move || Some(engine), None);
        sup.shared.restart_grace_ms.store(50, Ordering::SeqCst);

        // Idle until told what to capture.
        thread::sleep(Duration::from_millis(200));
        assert_eq!(sup.health(), EngineHealth::Stopped);
        assert_eq!(sup.shared.spawn_count.load(Ordering::SeqCst), 0);

        // More retargets than the give-up threshold (6): none may count.
        for n in 1..=8u32 {
            sup.retarget(Some(EngineSpec { env: vec![("RECALL_CAPTURE_NAME".into(), Some(format!("game-{n}")))] }));
            assert!(
                wait_for(5, || sup.shared.spawn_count.load(Ordering::SeqCst) == n
                    && matches!(sup.health(), EngineHealth::Running { pid } if pid != 0)),
                "retarget {n}: health {:?}, spawns {}",
                sup.health(),
                sup.shared.spawn_count.load(Ordering::SeqCst)
            );
        }
        assert_ne!(sup.health(), EngineHealth::GivenUp);

        // retarget(None) stops the engine and idles.
        sup.retarget(None);
        assert!(wait_for(5, || sup.health() == EngineHealth::Stopped));
        assert_eq!(sup.shared.spawn_count.load(Ordering::SeqCst), 8, "no respawn while idle");
        sup.shutdown(Duration::from_millis(50));
    }

    // --- LIVE test against the real engine --------------------------------
    //
    // Requires the real capture engine. It starts screen capture for a few
    // seconds. Ignored by default; run with `-- --ignored --nocapture`.

    fn real_engine_path() -> Option<PathBuf> {
        ["release", "debug"]
            .iter()
            .map(|profile| PathBuf::from(format!("L:/Projects/recall/target/{profile}/recall.exe")))
            .find(|p| p.is_file())
    }

    fn pid_alive(pid: u32) -> bool {
        match Command::new("tasklist").args(["/FI", &format!("PID eq {pid}"), "/NH"]).output() {
            Ok(o) => {
                let s = String::from_utf8_lossy(&o.stdout);
                s.contains(&pid.to_string()) && s.to_lowercase().contains("recall")
            }
            Err(_) => false,
        }
    }

    #[test]
    #[ignore = "live: spawns the real recall.exe (screen capture) for a few seconds"]
    fn live_spawn_hello_shutdown_no_orphan() {
        let Some(engine) = real_engine_path() else {
            panic!("real recall.exe not found under L:/Projects/recall/target");
        };
        let sup = Supervisor::start_with_locator(move || Some(engine), Some(EngineSpec::default()));
        let mut pid = 0;
        assert!(wait_for(5, || {
            if let EngineHealth::Running { pid: p } = sup.health() {
                pid = p;
            }
            pid != 0
        }));
        assert!(pid_alive(pid), "OS confirms engine pid {pid} is alive");
        assert!(wait_for(5, || sup.hello_count() > 0), "no hello parsed within 5s");
        assert!(!sup.cached_engine_events().is_empty(), "hello cached for late joiners");
        sup.shutdown(Duration::from_secs(3));
        assert!(wait_for(3, || !pid_alive(pid)), "orphan: engine pid {pid} still alive");
    }
}
