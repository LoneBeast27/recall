//! The manager thread: spawn the engine, supervise it until it exits / a stop
//! / a retarget arrives, run the ordered quit -> grace -> kill stop, and apply
//! the restart policy to unintended exits.

use std::path::{Path, PathBuf};
use std::process::Child;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use super::protocol::{drain_stderr, read_protocol};
use super::{engine_command, EngineHealth, Shared, SHUTDOWN_POLL, SLEEP_CHUNK};
use crate::engine_policy::{EngineSpec, Next, RestartPolicy, GIVEUP_WINDOW};
use crate::record_dispatch::Sent;
use crate::log;

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
pub(super) fn run_manager(shared: Arc<Shared>, engine_path: Option<PathBuf>) {
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
            shared.end_quitting(gen); // stopped to Off: no engine follows
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
                // Shutdown is no game switch: a line sent meanwhile is told "not responding".
                // A retarget's reason is read with its quit (`None`: `send_retarget_quit`).
                let stop = match exit {
                    ChildExit::StopRequested => Some((shared.grace(), Some(Sent::NotResponding))),
                    ChildExit::Retarget => Some((shared.restart_grace(), None)),
                    _ => None,
                };
                if let Some((grace, refusal)) = stop {
                    // Run quit->grace->kill FIRST, while the stdin queue is open (`quit` is queued,
                    // never written here: a wedged pipe cannot hold the kill back);
                    // only a dead child closes stdout and unblocks the reader.
                    finish_shutdown(&shared, child, grace, refusal);
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
                    ChildExit::Clean(code) => match crate::engine_exit::fatal_reason(code) {
                        // A broken install: every retry would fail the same way.
                        Some(why) => {
                            log!("supervisor: engine could not start (code {:#x}): {why}; not restarting until the target changes.", code as u32);
                            shared.set_health(EngineHealth::GivenUp);
                            if !wait_for_change(&shared, gen) {
                                break;
                            }
                            continue;
                        }
                        None => log!("supervisor: engine exited (code {code})"),
                    },
                    ChildExit::Signalled => log!("supervisor: engine terminated without an exit code"),
                }
            }
            Err(e) => {
                log!("supervisor: failed to spawn engine: {e}");
                shared.end_quitting(gen); // no engine came up
            }
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

/// Ordered stop of a still-running child: send `quit` (from then on every line is
/// refused with `refusal`, `None` = the last retarget's, until the next engine's stdin),
/// wait up to `grace` polling `try_wait`, then hard-kill any survivor.
fn finish_shutdown(shared: &Arc<Shared>, mut child: Child, grace: Duration, refusal: Option<Sent>) {
    let pid = child.id();
    // First, even for a child already gone: no line may go to an engine that is ending.
    match refusal {
        Some(refusal) => shared.send_quit(refusal),
        None => shared.send_retarget_quit(),
    };
    if let Ok(Some(_)) = child.try_wait() {
        log!("supervisor: engine (pid {pid}) already exited before shutdown");
        return;
    }
    log!("supervisor: stopping engine (pid {pid}); grace {}ms", grace.as_millis());
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
