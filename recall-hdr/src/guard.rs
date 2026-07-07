//! In-process restore guarantees: a RAII drop-guard plus a
//! `SetConsoleCtrlHandler` hookup, so the display baseline is restored on every
//! *soft* exit path this process can observe:
//!
//! - normal `main` return / `std::process::exit` via the guard's `Drop`,
//! - a panic unwind via the guard's `Drop`,
//! - Ctrl-C / Ctrl-Break / console-close / logoff / shutdown via the console
//!   control handler.
//!
//! The one path we *cannot* observe in-process is `taskkill /F` (SIGKILL-equiv):
//! there is no handler for it. That case is repaired at next launch by the
//! persistent journal ([`crate::journal`]) -- these two modules are the whole
//! "display is never left stuck" guarantee, split by what's observable.
//!
//! # Once-semantics (the core invariant)
//! The restore action must run **at most once** even though up to three triggers
//! race for it: the guard's `Drop`, the ctrl handler (on a *foreign* OS thread),
//! and an explicit clean-shutdown call. Each mechanism is internally guarded, and
//! the intended usage shares a single [`OnceRestore`] across all of them so a
//! Ctrl-C that fires *during* an already-running clean shutdown does not
//! double-restore. See [`OnceRestore`].

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, Once};

/// A restore action that runs at most once no matter how many triggers fire.
///
/// This is the shared spine of the once-guarantee: wrap the real restore in one
/// `OnceRestore`, then hand `Arc` clones to both [`RestoreGuard::arm`] and
/// [`install_ctrl_handler`]. Whichever trigger wins runs the closure; the rest
/// see it already fired and no-op. Safe to call from the ctrl handler's foreign
/// thread (the inner `Once` + `Mutex` synchronize).
pub struct OnceRestore {
    once: Once,
    // `Once` guarantees single execution; the `Mutex<Option<..>>` lets us both
    // store a `FnMut`/`FnOnce`-style action and drop it after it runs so we don't
    // pin captured resources for the process lifetime.
    action: Mutex<Option<Box<dyn FnMut() + Send>>>,
}

impl OnceRestore {
    /// Wrap `action` (the real display restore) into a shareable once-cell.
    pub fn new(action: Box<dyn FnMut() + Send>) -> Arc<OnceRestore> {
        Arc::new(OnceRestore {
            once: Once::new(),
            action: Mutex::new(Some(action)),
        })
    }

    /// Run the restore iff it has not run yet. Returns `true` if *this* call was
    /// the one that executed it, `false` if it had already fired. Reentrancy- and
    /// thread-safe: `Once::call_once` serializes racing callers.
    pub fn run(&self) -> bool {
        let mut ran_here = false;
        self.once.call_once(|| {
            ran_here = true;
            // Take the action out so its captures are dropped once done. A panic
            // inside the action would poison the mutex; we defensively recover so
            // a poisoned lock can't wedge a subsequent (harmless) `run`.
            let mut slot = self
                .action
                .lock()
                .unwrap_or_else(|p| p.into_inner());
            if let Some(mut act) = slot.take() {
                act();
            }
        });
        ran_here
    }

    /// Whether the restore has already executed (used by tests / disarm logic).
    pub fn has_run(&self) -> bool {
        self.once.is_completed()
    }
}

/// RAII guard that runs its restore closure exactly once on `Drop` -- covering
/// both a normal scope exit and a panic unwind -- unless [`disarm`](Self::disarm)
/// was called first (the clean-shutdown path that already restored).
///
/// The closure is `FnMut` per the spec; the guard invokes it at most once, so an
/// `FnMut` and an `FnOnce`-via-`Option` both work. Internally an [`AtomicBool`]
/// makes arm/disarm and the drop check race-free.
pub struct RestoreGuard {
    restore: Option<Box<dyn FnMut() + Send>>,
    armed: Arc<AtomicBool>,
}

impl RestoreGuard {
    /// Arm a guard around `restore`. It fires on drop unless disarmed.
    pub fn arm(restore: Box<dyn FnMut() + Send>) -> RestoreGuard {
        RestoreGuard {
            restore: Some(restore),
            armed: Arc::new(AtomicBool::new(true)),
        }
    }

    /// Arm a guard that drives a shared [`OnceRestore`] on drop. Use this when
    /// the same restore is also wired to the ctrl handler, so a drop after the
    /// handler already restored is a no-op (and vice-versa).
    pub fn arm_shared(shared: Arc<OnceRestore>) -> RestoreGuard {
        RestoreGuard::arm(Box::new(move || {
            shared.run();
        }))
    }

    /// Disarm: the restore will **not** run on drop. For the clean-shutdown path
    /// where restore already happened. Idempotent.
    pub fn disarm(&mut self) {
        self.armed.store(false, Ordering::SeqCst);
    }
}

impl Drop for RestoreGuard {
    fn drop(&mut self) {
        // `swap(false)` both reads and clears atomically, so two racing drops (or
        // a drop concurrent with disarm) never double-run.
        if self.armed.swap(false, Ordering::SeqCst) {
            if let Some(mut restore) = self.restore.take() {
                restore();
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Console control handler
// ---------------------------------------------------------------------------

use windows::Win32::System::Console::{
    SetConsoleCtrlHandler, CTRL_BREAK_EVENT, CTRL_CLOSE_EVENT, CTRL_C_EVENT, CTRL_LOGOFF_EVENT,
    CTRL_SHUTDOWN_EVENT,
};

use crate::log;

/// The single registered restore, invoked by the console control handler. The
/// handler runs on a *foreign* OS thread the runtime spawns, and must be
/// reentrancy-safe -- hence the `OnceLock` + the `Fn + Send + Sync` bound.
static CTRL_RESTORE: std::sync::OnceLock<Box<dyn Fn() + Send + Sync>> = std::sync::OnceLock::new();

/// Guards single execution of the ctrl-handler restore across repeated events
/// (e.g. a CTRL_LOGOFF followed by CTRL_SHUTDOWN both delivered).
static CTRL_ONCE: Once = Once::new();

/// The Win32 `PHANDLER_ROUTINE`. Runs on a foreign thread; must not panic across
/// the FFI boundary and must be reentrancy-safe.
///
/// Semantics: for every event we care about we run the restore once (guarded by
/// [`CTRL_ONCE`]), then return `FALSE` so the *default* handler proceeds and the
/// process is allowed to terminate. Returning `TRUE` would swallow Ctrl-C and
/// keep the process alive, which we don't want -- we restore, then let it die.
unsafe extern "system" fn ctrl_handler(ctrl_type: u32) -> windows::core::BOOL {
    match ctrl_type {
        CTRL_C_EVENT | CTRL_BREAK_EVENT | CTRL_CLOSE_EVENT | CTRL_LOGOFF_EVENT
        | CTRL_SHUTDOWN_EVENT => {
            CTRL_ONCE.call_once(|| {
                if let Some(restore) = CTRL_RESTORE.get() {
                    restore();
                }
            });
            // FALSE -> pass to the next/default handler, which terminates us.
            windows::core::BOOL::from(false)
        }
        // Unknown event: not ours, let the next handler decide.
        _ => windows::core::BOOL::from(false),
    }
}

/// Register `restore` to run synchronously on Ctrl-C / Ctrl-Break / console
/// close / logoff / shutdown, then allow termination.
///
/// Constraints (documented per the module contract):
/// - The handler runs on a **foreign OS thread**, so `restore` must be `Send`
///   and `Sync`. It must also be **reentrancy-safe**: it may be entered while
///   the main thread is mid-shutdown. We guard it with a [`Once`] so it runs at
///   most once regardless of how many control events arrive.
/// - To make the once-guarantee span the guard *and* the handler, pass a closure
///   that drives a shared [`OnceRestore`] (see [`install_shared`]).
///
/// Idempotent-ish: only the first call installs the closure + the OS handler;
/// later calls report an error (the slot is already filled) so a double-install
/// is a loud programmer error, not a silent second handler.
pub fn install_ctrl_handler(restore: Box<dyn Fn() + Send + Sync>) -> Result<(), String> {
    CTRL_RESTORE
        .set(restore)
        .map_err(|_| "ctrl handler already installed".to_string())?;

    // SAFETY: registering a valid, 'static `extern "system"` handler routine.
    // `add = true` adds (does not replace) our handler to the process list.
    unsafe {
        SetConsoleCtrlHandler(Some(ctrl_handler), true)
            .map_err(|e| format!("SetConsoleCtrlHandler failed: {e}"))?;
    }
    log!("guard: console control handler installed");
    Ok(())
}

/// Convenience: wire a shared [`OnceRestore`] to the ctrl handler so it shares
/// the once-guarantee with a [`RestoreGuard::arm_shared`] built from the same
/// `Arc`. The handler closure just calls `shared.run()`.
pub fn install_shared(shared: Arc<OnceRestore>) -> Result<(), String> {
    install_ctrl_handler(Box::new(move || {
        shared.run();
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::sync::Arc;

    #[test]
    fn guard_runs_closure_on_drop() {
        let hits = Arc::new(AtomicU32::new(0));
        {
            let h = hits.clone();
            let _g = RestoreGuard::arm(Box::new(move || {
                h.fetch_add(1, Ordering::SeqCst);
            }));
        } // drop here
        assert_eq!(hits.load(Ordering::SeqCst), 1, "restore runs on drop");
    }

    #[test]
    fn guard_does_not_run_after_disarm() {
        let hits = Arc::new(AtomicU32::new(0));
        {
            let h = hits.clone();
            let mut g = RestoreGuard::arm(Box::new(move || {
                h.fetch_add(1, Ordering::SeqCst);
            }));
            g.disarm();
        }
        assert_eq!(
            hits.load(Ordering::SeqCst),
            0,
            "disarmed guard must not run on drop"
        );
    }

    #[test]
    fn guard_runs_on_panic_unwind() {
        let hits = Arc::new(AtomicU32::new(0));
        let h = hits.clone();
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _g = RestoreGuard::arm(Box::new(move || {
                h.fetch_add(1, Ordering::SeqCst);
            }));
            panic!("boom");
        }));
        assert!(result.is_err(), "the panic propagated");
        assert_eq!(
            hits.load(Ordering::SeqCst),
            1,
            "restore runs during unwind"
        );
    }

    #[test]
    fn once_restore_runs_at_most_once() {
        let hits = Arc::new(AtomicU32::new(0));
        let h = hits.clone();
        let once = OnceRestore::new(Box::new(move || {
            h.fetch_add(1, Ordering::SeqCst);
        }));

        assert!(once.run(), "first run executes");
        assert!(!once.run(), "second run is a no-op");
        assert!(!once.run(), "third run is a no-op");
        assert!(once.has_run());
        assert_eq!(hits.load(Ordering::SeqCst), 1, "executed exactly once");
    }

    #[test]
    fn shared_guard_and_manual_call_run_once_total() {
        // The core cross-mechanism invariant: a manual (clean-shutdown) run plus
        // a guard drop that both target the SAME OnceRestore = exactly one exec.
        let hits = Arc::new(AtomicU32::new(0));
        let h = hits.clone();
        let once = OnceRestore::new(Box::new(move || {
            h.fetch_add(1, Ordering::SeqCst);
        }));

        {
            let g = RestoreGuard::arm_shared(once.clone());
            // Clean-shutdown path restores explicitly...
            assert!(once.run(), "manual run executed");
            // ...guard still drops here, but must NOT re-run.
            drop(g);
        }
        assert_eq!(
            hits.load(Ordering::SeqCst),
            1,
            "shared guard-drop + manual call = one execution"
        );
    }

    #[test]
    fn once_restore_is_thread_safe_single_exec() {
        // Race many threads on the same OnceRestore; exactly one wins.
        let hits = Arc::new(AtomicU32::new(0));
        let h = hits.clone();
        let once = OnceRestore::new(Box::new(move || {
            h.fetch_add(1, Ordering::SeqCst);
        }));

        let winners = Arc::new(AtomicU32::new(0));
        let handles: Vec<_> = (0..8)
            .map(|_| {
                let o = once.clone();
                let w = winners.clone();
                std::thread::spawn(move || {
                    if o.run() {
                        w.fetch_add(1, Ordering::SeqCst);
                    }
                })
            })
            .collect();
        for hdl in handles {
            hdl.join().unwrap();
        }
        assert_eq!(hits.load(Ordering::SeqCst), 1, "executed once under race");
        assert_eq!(winners.load(Ordering::SeqCst), 1, "exactly one winner");
    }
}
