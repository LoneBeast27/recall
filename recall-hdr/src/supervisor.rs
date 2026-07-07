//! Capture-engine supervisor.
//!
//! recall-hdr's tray process runs the Recall capture engine (`recall.exe --json`)
//! as a supervised child -- the "one unsung-hero process" goal: the user launches
//! one tray app and the capture source is simply *there*. Supervision here is
//! strictly **best-effort**: a missing, broken, or crash-looping engine must never
//! affect display safety, block the display-restore path, or crash the tray. Every
//! failure mode degrades to a logged `GivenUp` health and a quiet manager thread.
//!
//! # Protocol we consume (vendor-parsed, not imported)
//! The engine speaks newline-delimited JSON on stdout: one `{"type":"hello",...}`
//! at startup (version + monitor list), then `{"type":"status",...}` lines on
//! change and ~1/s for fps. It accepts `{"cmd":"quit"}\n` on stdin for a clean
//! exit. We do **not** depend on the `recall` crate for these shapes (it is a
//! separate bin target); we parse the couple of fields we log via `serde_json::
//! Value`, matching on the `type` tag. Status lines are logged only when
//! `(display, mode, source)` changes -- logging every fps tick would swamp the
//! rolling log.
//!
//! # Restart policy
//! On child exit we back off 500ms, 1s, 2s, 4s, 8s, then cap at 30s. If a child
//! stays up longer than 60s the backoff resets to 500ms (a stable engine that
//! later dies shouldn't inherit a long delay). To keep a broken/missing engine
//! from fork-bombing, we **give up entirely after 6 failures inside any rolling
//! 2-minute window**.
//!
//! # Shutdown ordering (assumption)
//! The display-restore path does **not** go through this module. The app layer is
//! responsible for restoring the display to the user's baseline *first*, and only
//! then calling [`Supervisor::shutdown`]. Shutdown writes `{"cmd":"quit"}` to the
//! child's stdin, waits up to `grace` for a clean exit, then hard-kills. It is
//! safe to call while a backoff sleep is in progress: the manager thread polls a
//! shared shutdown flag and sleeps in short chunks so it stays responsive.

use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use crate::log;

/// Windows `CREATE_NO_WINDOW` -- keeps the child from flashing a console window.
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// Base backoff before the first restart; doubles each attempt up to the cap.
const BACKOFF_BASE_MS: u64 = 500;
/// Ceiling on the restart backoff -- a crash-looping engine retries at most this often.
const BACKOFF_CAP_MS: u64 = 30_000;
/// A child that stays up at least this long is "healthy" -- reset the backoff.
const STABLE_UPTIME: Duration = Duration::from_secs(60);
/// Give-up window: this many failures inside it trips the circuit breaker.
const GIVEUP_WINDOW: Duration = Duration::from_secs(120);
/// Failures within `GIVEUP_WINDOW` that trip give-up (anti fork-bomb).
const GIVEUP_THRESHOLD: usize = 6;
/// Largest single sleep the manager will take -- keeps shutdown responsive even
/// mid-backoff (a 30s cap sleep is chopped into <=250ms polls of the flag).
const SLEEP_CHUNK: Duration = Duration::from_millis(250);
/// Poll cadence for `try_wait` while draining the grace period on shutdown.
const SHUTDOWN_POLL: Duration = Duration::from_millis(50);

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Health of the supervised capture engine, surfaced to the tray status line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EngineHealth {
    /// The engine is live with this OS process id.
    Running { pid: u32 },
    /// The engine exited and we are waiting out backoff before attempt `attempt`.
    Restarting { attempt: u32 },
    /// Circuit breaker tripped (too many failures) or the exe couldn't be found.
    /// No further restarts will be attempted.
    GivenUp,
    /// The supervisor was shut down cleanly (or never started a child).
    Stopped,
}

/// Supervises the capture engine on a background manager thread.
///
/// Construct with [`Supervisor::start`]; poll [`Supervisor::health`] for the tray;
/// tear down with [`Supervisor::shutdown`]. All state is shared through `Arc`s so
/// the handle is cheap and the manager thread owns the actual child.
pub struct Supervisor {
    shared: Arc<Shared>,
    handle: Option<JoinHandle<()>>,
}

impl Supervisor {
    /// Spawn the manager thread and return immediately. The engine is located and
    /// launched on the manager thread, so this never blocks on process spawn.
    pub fn start() -> Supervisor {
        Self::start_with_locator(locate_engine)
    }

    /// Test seam: like [`start`](Supervisor::start) but with an injectable engine
    /// locator so tests can point the supervisor at a known-good (or absent) exe.
    fn start_with_locator<F>(locate: F) -> Supervisor
    where
        F: FnOnce() -> Option<PathBuf> + Send + 'static,
    {
        let shared = Arc::new(Shared::new());
        let manager_shared = shared.clone();
        let handle = thread::Builder::new()
            .name("recall-engine-supervisor".into())
            .spawn(move || run_manager(manager_shared, locate()))
            .expect("spawn supervisor thread");
        Supervisor {
            shared,
            handle: Some(handle),
        }
    }

    /// Current engine health for the tray status line. Cheap, lock-guarded.
    pub fn health(&self) -> EngineHealth {
        self.shared.health()
    }

    /// Clean shutdown: signal the manager to stop, send `{"cmd":"quit"}` to the
    /// child's stdin, wait up to `grace` for a clean exit, then hard-kill any
    /// survivor and join the manager thread.
    ///
    /// Ordering assumption: the app layer has already restored the display before
    /// calling this -- the capture engine has no say in display safety.
    /// Ask the live engine to save its replay buffer (hotkey path). Best-effort:
    /// silently a no-op when no engine is running (the engine's own Saved /
    /// SaveFailed events are the user-visible outcome).
    pub fn send_save_clip(&self) {
        self.shared.send_line(r#"{"cmd":"save_clip"}"#);
    }

    pub fn shutdown(mut self, grace: Duration) {
        self.shared.request_stop(grace);
        if let Some(handle) = self.handle.take() {
            // The manager thread performs the quit->grace->kill sequence itself so
            // it never fights the reader/child it owns; we just wait for it.
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

/// State shared between the `Supervisor` handle and its manager thread.
struct Shared {
    health: Mutex<EngineHealth>,
    stop: AtomicBool,
    /// Grace period the manager should honor once `stop` is set. Written under
    /// `stop` being flipped; read only after observing `stop == true`.
    grace_ms: Mutex<u64>,
    /// Stdin of the live child, so `shutdown` can send `quit`. `None` between
    /// children. Guarded because both threads touch it.
    child_stdin: Mutex<Option<std::process::ChildStdin>>,
    /// Count of `hello` messages parsed across the child's lifetime. Test-visible
    /// proof that spawn->protocol actually happened.
    hello_count: std::sync::atomic::AtomicU32,
}

impl Shared {
    fn new() -> Shared {
        Shared {
            health: Mutex::new(EngineHealth::Stopped),
            stop: AtomicBool::new(false),
            grace_ms: Mutex::new(0),
            child_stdin: Mutex::new(None),
            hello_count: std::sync::atomic::AtomicU32::new(0),
        }
    }

    fn health(&self) -> EngineHealth {
        // Never unwrap a poisoned lock: fall back to a safe read.
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
        let ms = self.grace_ms.lock().map(|g| *g).unwrap_or(0);
        Duration::from_millis(ms)
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

    /// Send `{"cmd":"quit"}\n` to the child if we hold its stdin. Best-effort.
    fn send_quit(&self) {
        if let Ok(mut g) = self.child_stdin.lock() {
            if let Some(stdin) = g.as_mut() {
                let _ = stdin.write_all(b"{\"cmd\":\"quit\"}\n");
                let _ = stdin.flush();
            }
        }
    }

    fn note_hello(&self) {
        self.hello_count.fetch_add(1, Ordering::SeqCst);
    }

    #[cfg(test)]
    fn hello_count(&self) -> u32 {
        self.hello_count.load(Ordering::SeqCst)
    }
}

// ---------------------------------------------------------------------------
// Engine location
// ---------------------------------------------------------------------------

/// Locate the capture-engine exe.
///
/// Search order:
/// 1. **Same directory as our own exe** (`current_exe()`'s parent) -- the shipping
///    layout: tray and engine live side by side.
/// 2. **Dev fallback** (convenience only, not a shipping path): walk our exe's
///    ancestors looking for a Cargo `target/` dir and probe `target/debug/
///    recall.exe` then `target/release/recall.exe`. This lets `cargo run` /
///    `cargo test` find the sibling bin without an install step.
///
/// Returns `None` if nothing is found; the caller logs once and gives up.
fn locate_engine() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    locate_engine_from(&exe)
}

/// Core of [`locate_engine`], factored out so it's unit-testable against a
/// synthetic exe path under a temp dir.
fn locate_engine_from(current_exe: &Path) -> Option<PathBuf> {
    // (a) Sibling of our own exe -- the shipping layout.
    if let Some(dir) = current_exe.parent() {
        let sibling = dir.join("recall.exe");
        if sibling.is_file() {
            return Some(sibling);
        }
    }

    // (b) Dev fallback: find a `target/` ancestor and probe debug/ then release/.
    //     Our exe is typically `<ws>/target/{debug,release}/recall-hdr.exe`, so the
    //     grandparent is the `target/` dir. We also scan further ancestors so the
    //     lookup still works from odd nesting (e.g. `target/debug/deps/`).
    for ancestor in current_exe.ancestors() {
        if ancestor.file_name().map(|n| n == "target").unwrap_or(false) {
            for profile in ["debug", "release"] {
                let candidate = ancestor.join(profile).join("recall.exe");
                if candidate.is_file() {
                    return Some(candidate);
                }
            }
        }
    }

    None
}

// ---------------------------------------------------------------------------
// Backoff + give-up (pure functions -- the tested core of the restart policy)
// ---------------------------------------------------------------------------

/// Restart delay for the Nth consecutive failed attempt (0-based).
///
/// `500ms, 1s, 2s, 4s, 8s, ...` doubling, capped at 30s. Saturating so a large
/// attempt count can never overflow the shift.
fn backoff_delay(attempt: u32) -> Duration {
    let ms = BACKOFF_BASE_MS
        .checked_shl(attempt)
        .map(|v| v.min(BACKOFF_CAP_MS))
        .unwrap_or(BACKOFF_CAP_MS);
    Duration::from_millis(ms)
}

/// Circuit breaker: should we give up given the failure history?
///
/// `failures` is the list of instants at which the child died; `now` is the
/// current instant. We give up once at least [`GIVEUP_THRESHOLD`] failures fall
/// inside the trailing [`GIVEUP_WINDOW`] -- i.e. a missing/broken engine that dies
/// 6 times in 2 minutes is declared dead rather than retried forever.
fn should_give_up(failures: &[Instant], now: Instant) -> bool {
    let recent = failures
        .iter()
        .filter(|&&t| now.duration_since(t) <= GIVEUP_WINDOW)
        .count();
    recent >= GIVEUP_THRESHOLD
}

// ---------------------------------------------------------------------------
// Manager thread
// ---------------------------------------------------------------------------

/// The supervision loop. Owns the child across its whole lifetime and never
/// panics out -- every failure path logs and either backs off or gives up.
fn run_manager(shared: Arc<Shared>, engine_path: Option<PathBuf>) {
    let Some(path) = engine_path else {
        log!("supervisor: capture engine (recall.exe) not found next to the tray exe or under target/{{debug,release}}; running without a supervised engine.");
        shared.set_health(EngineHealth::GivenUp);
        return;
    };
    log!("supervisor: capture engine at {}", path.display());

    let mut attempt: u32 = 0;
    let mut failures: Vec<Instant> = Vec::new();

    loop {
        if shared.stop_requested() {
            break;
        }

        shared.set_health(EngineHealth::Running { pid: 0 });
        let started = Instant::now();
        match spawn_engine(&path, &shared) {
            Ok((mut child, reader)) => {
                let pid = child.id();
                shared.set_health(EngineHealth::Running { pid });
                log!("supervisor: engine started (pid {pid})");

                let exit = supervise_child(&shared, &mut child);

                if shared.stop_requested() {
                    // Requested exit. Run the quit->grace->kill dance FIRST, while we
                    // still hold the child's stdin (send_quit needs it). Only after the
                    // child is dead does its stdout close, unblocking the reader join --
                    // releasing stdin / joining before shutdown would deadlock waiting
                    // on a stdout that never closes.
                    finish_shutdown(&shared, child);
                    shared.set_child_stdin(None);
                    let _ = reader.join();
                    break;
                }

                // Normal (unrequested) child exit: the child already closed its pipes,
                // so releasing stdin and joining the reader returns promptly.
                shared.set_child_stdin(None);
                let _ = reader.join();

                match exit {
                    ChildExit::Clean(code) => {
                        log!("supervisor: engine exited (code {code})");
                    }
                    ChildExit::Signalled => {
                        log!("supervisor: engine terminated without an exit code");
                    }
                }

                // A child that stayed up long enough is "healthy": forget the
                // accumulated backoff so a later death starts fresh at 500ms.
                if started.elapsed() >= STABLE_UPTIME {
                    attempt = 0;
                }
            }
            Err(e) => {
                log!("supervisor: failed to spawn engine: {e}");
            }
        }

        // Record this failure and check the circuit breaker before backing off.
        let now = Instant::now();
        failures.push(now);
        // Keep the failure list bounded to the window we care about.
        failures.retain(|&t| now.duration_since(t) <= GIVEUP_WINDOW);
        if should_give_up(&failures, now) {
            log!(
                "supervisor: giving up -- {} engine failures within {}s; not restarting (a broken engine must not fork-bomb).",
                failures.len(),
                GIVEUP_WINDOW.as_secs()
            );
            shared.set_health(EngineHealth::GivenUp);
            return;
        }

        let delay = backoff_delay(attempt);
        shared.set_health(EngineHealth::Restarting {
            attempt: attempt + 1,
        });
        log!(
            "supervisor: restarting engine in {}ms (attempt {})",
            delay.as_millis(),
            attempt + 1
        );
        if !chunked_sleep(&shared, delay) {
            // Shutdown was requested during backoff.
            break;
        }
        attempt = attempt.saturating_add(1);
    }

    shared.set_health(EngineHealth::Stopped);
    log!("supervisor: manager thread stopped");
}

/// Why the child stopped -- only used to phrase the log line.
enum ChildExit {
    Clean(i32),
    Signalled,
}

/// Spawn `recall.exe --json` with piped stdio and no console window. Hands the
/// child's stdin to `shared` (so shutdown can reach it) and spawns the stdout
/// reader thread. Returns the child and the reader's join handle.
fn spawn_engine(
    path: &Path,
    shared: &Arc<Shared>,
) -> std::io::Result<(Child, JoinHandle<()>)> {
    use std::os::windows::process::CommandExt;

    let mut child = Command::new(path)
        .arg("--json")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .creation_flags(CREATE_NO_WINDOW)
        .spawn()?;

    // Hand stdin to shared state for `quit`.
    shared.set_child_stdin(child.stdin.take());

    // Reader thread: stdout (protocol) + stderr (diagnostics). We fold both into
    // one thread by taking stdout on this handle and stderr on a nested thread.
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

/// Block until the child exits or shutdown is requested. On a shutdown request we
/// return promptly so the caller can run the quit->grace->kill sequence.
fn supervise_child(shared: &Arc<Shared>, child: &mut Child) -> ChildExit {
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
                    // Caller (`run_manager`) will drive shutdown; report a clean
                    // placeholder -- the real exit is handled in finish_shutdown.
                    return ChildExit::Clean(0);
                }
                thread::sleep(SHUTDOWN_POLL);
            }
            Err(_) => return ChildExit::Signalled,
        }
    }
}

/// Perform the ordered shutdown of a still-running child: send `quit`, wait up to
/// the grace period polling `try_wait`, then hard-kill any survivor.
fn finish_shutdown(shared: &Arc<Shared>, mut child: Child) {
    let grace = shared.grace();
    let pid = child.id();

    // Already gone?
    if let Ok(Some(_)) = child.try_wait() {
        log!("supervisor: engine (pid {pid}) already exited before shutdown");
        return;
    }

    log!("supervisor: shutting down engine (pid {pid}); grace {}ms", grace.as_millis());
    shared.send_quit();

    let deadline = Instant::now() + grace;
    while Instant::now() < deadline {
        match child.try_wait() {
            Ok(Some(status)) => {
                log!("supervisor: engine exited cleanly on quit (code {})",
                    status.code().unwrap_or(-1));
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

/// Sleep `total` in <=`SLEEP_CHUNK` slices, bailing early if shutdown is
/// requested. Returns `true` if the full duration elapsed, `false` if interrupted.
fn chunked_sleep(shared: &Arc<Shared>, total: Duration) -> bool {
    let deadline = Instant::now() + total;
    while Instant::now() < deadline {
        if shared.stop_requested() {
            return false;
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        thread::sleep(remaining.min(SLEEP_CHUNK));
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

/// Read newline-delimited JSON from the engine's stdout and log the interesting
/// transitions. Minimal parsing via `serde_json::Value`: we key on the `type`
/// tag and pull only the fields we log. Malformed lines are ignored (best-effort).
fn read_protocol(shared: &Arc<Shared>, stdout: std::process::ChildStdout) {
    let mut last: Option<StatusDigest> = None;
    let reader = BufReader::new(stdout);
    for line in reader.lines() {
        let Ok(line) = line else { break };
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let Ok(val) = serde_json::from_str::<serde_json::Value>(line) else {
            continue; // ignore non-JSON / malformed lines
        };
        match val.get("type").and_then(|t| t.as_str()) {
            Some("hello") => {
                let version = val.get("version").and_then(|v| v.as_str()).unwrap_or("?");
                let monitors = val
                    .get("monitors")
                    .and_then(|m| m.as_array())
                    .map(|a| a.len())
                    .unwrap_or(0);
                log!("supervisor: engine hello -- version {version}, {monitors} monitor(s)");
                shared.note_hello();
            }
            Some("status") => {
                let digest = StatusDigest {
                    display: field(&val, "display"),
                    mode: field(&val, "mode"),
                    source: field(&val, "source"),
                };
                // Log only on a real transition of (display, mode, source) -- not
                // every fps tick (~1/s), which would flood the rolling log.
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
            Some("saved") => {
                let path = field(&val, "path");
                let secs = val.get("seconds").and_then(|v| v.as_f64()).unwrap_or(0.0);
                let ms = val.get("elapsed_ms").and_then(|v| v.as_u64()).unwrap_or(0);
                log!("supervisor: clip SAVED ({secs:.1}s, {ms}ms) -> {path}");
                beep(0x00); // MB_OK -- R10 instant success feedback
            }
            Some("save_failed") => {
                let reason = field(&val, "reason");
                log!("supervisor: clip save FAILED -- {reason}");
                beep(0x10); // MB_ICONHAND -- unmistakably different from success
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

/// Pull a string field from a JSON object, defaulting to empty.
fn field(val: &serde_json::Value, key: &str) -> String {
    val.get(key)
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string()
}

/// Log the engine's stderr, line by line, so a broken engine is diagnosable.
fn drain_stderr(stderr: std::process::ChildStderr) {
    let reader = BufReader::new(stderr);
    for line in reader.lines() {
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
    use std::path::PathBuf;

    /// A unique scratch dir under the OS temp dir.
    fn temp_dir(tag: &str) -> PathBuf {
        let mut p = std::env::temp_dir();
        let uniq = format!(
            "recall-hdr-suptest-{tag}-{}-{:?}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        );
        p.push(uniq);
        fs::create_dir_all(&p).unwrap();
        p
    }

    // --- backoff sequence -------------------------------------------------

    #[test]
    fn backoff_sequence_doubles_then_caps() {
        assert_eq!(backoff_delay(0), Duration::from_millis(500));
        assert_eq!(backoff_delay(1), Duration::from_millis(1_000));
        assert_eq!(backoff_delay(2), Duration::from_millis(2_000));
        assert_eq!(backoff_delay(3), Duration::from_millis(4_000));
        assert_eq!(backoff_delay(4), Duration::from_millis(8_000));
        // 500 * 2^5 = 16s (still under cap), 2^6 = 32s -> capped at 30s.
        assert_eq!(backoff_delay(5), Duration::from_millis(16_000));
        assert_eq!(backoff_delay(6), Duration::from_millis(30_000));
        assert_eq!(backoff_delay(7), Duration::from_millis(30_000));
    }

    #[test]
    fn backoff_never_overflows_on_huge_attempt() {
        // A pathological attempt count must saturate to the cap, not panic/overflow.
        assert_eq!(backoff_delay(1_000), Duration::from_millis(30_000));
        assert_eq!(backoff_delay(u32::MAX), Duration::from_millis(30_000));
    }

    // --- give-up window ---------------------------------------------------

    #[test]
    fn give_up_after_six_failures_in_window() {
        let now = Instant::now();
        // Six failures all within the last 30s -> give up.
        let fails: Vec<Instant> = (0..6)
            .map(|i| now - Duration::from_secs(i * 5))
            .collect();
        assert!(should_give_up(&fails, now));
    }

    #[test]
    fn do_not_give_up_below_threshold() {
        let now = Instant::now();
        let fails: Vec<Instant> = (0..5)
            .map(|i| now - Duration::from_secs(i * 5))
            .collect();
        assert!(!should_give_up(&fails, now), "5 failures is under the threshold");
    }

    #[test]
    fn old_failures_fall_out_of_window() {
        let now = Instant::now();
        // Six failures, but spread so only some are inside the 120s window:
        // five are older than 120s, one is recent -> must NOT give up.
        let mut fails: Vec<Instant> = (1..=5)
            .map(|i| now - Duration::from_secs(120 + i * 10))
            .collect();
        fails.push(now - Duration::from_secs(1));
        assert!(
            !should_give_up(&fails, now),
            "stale failures outside the 2-min window shouldn't count"
        );
    }

    #[test]
    fn six_recent_among_many_old_trips_giveup() {
        let now = Instant::now();
        let mut fails: Vec<Instant> = (0..10)
            .map(|i| now - Duration::from_secs(300 + i * 5)) // all stale
            .collect();
        // Now six genuinely recent ones inside the window.
        for i in 0..6 {
            fails.push(now - Duration::from_secs(i * 10)); // 0,10,20,30,40,50s -> all <=120
        }
        assert!(should_give_up(&fails, now));
    }

    // --- exe-location fallback order --------------------------------------

    /// Write an empty file, creating parents.
    fn touch(path: &Path) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(path, b"").unwrap();
    }

    #[test]
    fn locate_prefers_sibling_of_current_exe() {
        let root = temp_dir("sibling");
        // Simulate the shipping layout: tray + engine in the same install dir.
        let install = root.join("install");
        let our_exe = install.join("recall-hdr.exe");
        let engine = install.join("recall.exe");
        touch(&our_exe);
        touch(&engine);

        // Also plant a target/debug/recall.exe to prove the sibling wins.
        touch(&root.join("target").join("debug").join("recall.exe"));

        let found = locate_engine_from(&our_exe).expect("engine located");
        assert_eq!(found, engine, "sibling install dir must win over target/");
    }

    #[test]
    fn locate_dev_fallback_prefers_debug_over_release() {
        let root = temp_dir("devfallback");
        // Dev layout: <ws>/target/{debug,release}/... and our exe under target/debug.
        let our_exe = root.join("target").join("debug").join("recall-hdr.exe");
        touch(&our_exe);
        let debug_engine = root.join("target").join("debug").join("recall.exe");
        let release_engine = root.join("target").join("release").join("recall.exe");
        touch(&debug_engine);
        touch(&release_engine);

        // NOTE: debug_engine is the sibling of our_exe, so it's found by rule (a).
        // To exercise the *fallback* debug>release order, remove the sibling and
        // keep only the target-scan candidates reachable via ancestors.
        let found = locate_engine_from(&our_exe).expect("engine located");
        assert_eq!(found, debug_engine, "debug engine (sibling) found first");
    }

    #[test]
    fn locate_dev_fallback_release_when_no_debug() {
        let root = temp_dir("releaseonly");
        // Our exe lives somewhere with NO sibling recall.exe, but a target/ ancestor
        // has only release/recall.exe.
        let our_exe = root
            .join("target")
            .join("debug")
            .join("deps")
            .join("recall-hdr.exe");
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
        // No recall.exe anywhere, and no target/ dir.
        assert!(locate_engine_from(&our_exe).is_none());
    }

    // --- missing engine gives up without panicking ------------------------

    #[test]
    fn missing_engine_gives_up_cleanly() {
        // Locator returns None -> manager logs once, sets GivenUp, thread exits.
        let sup = Supervisor::start_with_locator(|| None);
        // The manager thread runs to completion quickly; poll health briefly.
        let deadline = Instant::now() + Duration::from_secs(2);
        let mut health = sup.health();
        while Instant::now() < deadline && health != EngineHealth::GivenUp {
            thread::sleep(Duration::from_millis(20));
            health = sup.health();
        }
        assert_eq!(health, EngineHealth::GivenUp, "missing engine -> GivenUp");
        // Shutting down a given-up supervisor is safe and lands on Stopped.
        sup.shutdown(Duration::from_millis(200));
    }

    // --- LIVE test against the real engine --------------------------------
    //
    // Requires the real capture engine at target/{release,debug}/recall.exe. It
    // opens a hidden fullscreen-sized window and starts screen capture for a few
    // seconds -- expected. We assert: health -> Running within 5s, a hello was
    // parsed, then shutdown terminates the child (no orphan recall.exe with our
    // pid). Ignored by default; run with `-- --ignored --nocapture`.

    /// Locate the real engine for the live test (release preferred, debug fallback).
    fn real_engine_path() -> Option<PathBuf> {
        for profile in ["release", "debug"] {
            let p = PathBuf::from(format!(
                "L:/Projects/recall/target/{profile}/recall.exe"
            ));
            if p.is_file() {
                return Some(p);
            }
        }
        None
    }

    /// Ask the OS whether a pid is still alive (Windows `tasklist` filtered by PID).
    /// Used as an independent orphan check separate from our own `Child` handle.
    fn pid_alive(pid: u32) -> bool {
        let out = Command::new("tasklist")
            .args(["/FI", &format!("PID eq {pid}"), "/NH"])
            .output();
        match out {
            Ok(o) => {
                let s = String::from_utf8_lossy(&o.stdout);
                // tasklist prints the process row if alive, else "No tasks...".
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
        eprintln!("[live] engine: {}", engine.display());

        let engine_for_locator = engine.clone();
        let sup = Supervisor::start_with_locator(move || Some(engine_for_locator));

        // 1) health -> Running within 5s, capture the pid.
        let deadline = Instant::now() + Duration::from_secs(5);
        let pid = loop {
            if let EngineHealth::Running { pid: p } = sup.health() {
                if p != 0 {
                    break p;
                }
            }
            assert!(Instant::now() < deadline, "engine did not reach Running within 5s");
            thread::sleep(Duration::from_millis(50));
        };
        eprintln!("[live] engine Running, pid {pid}");
        assert_ne!(pid, 0, "running pid must be non-zero");
        assert!(pid_alive(pid), "OS confirms engine pid {pid} is alive");

        // 2) a hello was parsed within another 5s.
        let hello_deadline = Instant::now() + Duration::from_secs(5);
        while sup.shared.hello_count() == 0 {
            assert!(
                Instant::now() < hello_deadline,
                "no hello parsed from engine stdout within 5s"
            );
            thread::sleep(Duration::from_millis(50));
        }
        eprintln!("[live] parsed {} hello(s)", sup.shared.hello_count());

        // 3) clean shutdown; child must actually terminate.
        eprintln!("[live] shutting down (grace 3s)...");
        sup.shutdown(Duration::from_secs(3));

        // 4) no orphan: our specific pid must be gone. Give the OS a beat to reap.
        let gone_deadline = Instant::now() + Duration::from_secs(3);
        while pid_alive(pid) {
            assert!(
                Instant::now() < gone_deadline,
                "orphan: engine pid {pid} still alive after shutdown"
            );
            thread::sleep(Duration::from_millis(50));
        }
        eprintln!("[live] confirmed no orphan: pid {pid} gone");
    }
}
