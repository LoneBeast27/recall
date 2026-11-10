//! The quit window's refusal: why a line sent while the engine quits is not written
//! (split out of `supervisor.rs`).
//!
//! The refusal follows the latest target, not the one the quit was sent for: a pause
//! then a resume inside the grace, or a drain to Off then a game taking focus, wants a
//! game again, so a line says "Switching games", not "Recording is paused" or "waiting
//! for a game". Lock order, here and nowhere reversed: `desired`, then `child_stdin`.

use super::Shared;
use crate::engine_policy::EngineSpec;
use crate::record_dispatch::Sent;

impl Shared {
    /// `refusal`: what a line sent while the old engine quits is told; a stop (`None`) is
    /// [`Sent::Paused`] for a pause, else always [`Sent::Waiting`]: no engine follows.
    /// An engine already quitting (not for shutdown) refuses with the new reason from now.
    pub(super) fn set_desired(&self, spec: Option<EngineSpec>, refusal: Sent) {
        self.set_desired_how(spec, refusal, false);
    }

    /// [`Self::set_desired`]; `release`: the running engine is released, not quit
    /// (`supervisor_release.rs`), noted under the same lock as the new generation.
    pub(super) fn set_desired_how(&self, spec: Option<EngineSpec>, refusal: Sent, release: bool) {
        let refusal = stop_refusal(spec.is_none(), refusal);
        let mut g = self.desired.lock().unwrap_or_else(|p| p.into_inner());
        g.0 += 1;
        if release {
            self.release_gen.store(g.0, std::sync::atomic::Ordering::SeqCst);
        }
        g.1 = spec;
        g.2 = refusal;
        let mut stdin = self.child_stdin.lock().unwrap_or_else(|p| p.into_inner());
        // Read under the lock: shutdown's quit ("isn't responding") is set after the stop
        // flag and under this lock, so a late retarget never replaces it.
        if stdin.quitting.is_some() && !self.stop_requested() {
            stdin.quitting = Some(refusal);
        }
    }

    /// A resume: a pause's stop is now a stop with no game. If no game takes the
    /// engine, a line meanwhile says "waiting for a game", not "Recording is paused".
    pub(super) fn unpause(&self) {
        let mut g = self.desired.lock().unwrap_or_else(|p| p.into_inner());
        if g.2 != Sent::Paused {
            return; // a game, a restart or shutdown since: its refusal stands
        }
        g.2 = Sent::Waiting;
        let mut stdin = self.child_stdin.lock().unwrap_or_else(|p| p.into_inner());
        if stdin.quitting == Some(Sent::Paused) {
            stdin.quitting = Some(Sent::Waiting);
        }
    }

    /// The quit window ended with no engine (a stop to Off, a failed respawn) for
    /// generation `gen`: the refusal ends with it, and a line now says the engine is
    /// gone, not why it quit. A target set since keeps its own refusal until its spawn
    /// (checked under the `desired` lock, so none lands in between).
    pub(super) fn end_quitting(&self, gen: u64) {
        let g = self.desired.lock().unwrap_or_else(|p| p.into_inner());
        if g.0 == gen {
            self.child_stdin.lock().unwrap_or_else(|p| p.into_inner()).quitting = None;
        }
    }

    /// `quit` takes the queue's reserved slot: a healthy engine behind a full queue still
    /// gets its clean finish (`supervisor_stdin.rs`). From here on every line is refused
    /// with `refusal` (set under the same lock), until the next engine's stdin.
    /// Shutdown is no game switch: once its stop flag is set (read under this lock, as
    /// [`Self::shutdown_refusal`] writes under it) a retarget's quit says "isn't responding"
    /// ([`at_shutdown`]).
    pub(super) fn send_quit(&self, refusal: Sent) -> bool {
        let mut g = self.child_stdin.lock().unwrap_or_else(|p| p.into_inner());
        g.quitting = Some(if self.stop_requested() { at_shutdown(refusal) } else { refusal });
        g.queue.as_ref().is_some_and(|queue| queue.send_quit(super::stdin::QUIT_LINE))
    }

    /// Shutdown, after its stop flag: a retarget's quit window already open ("Switching
    /// games", "Recorder is restarting") now says "isn't responding" until the engine is
    /// gone; a pause's or a stop's refusal stands ([`at_shutdown`]).
    pub(super) fn shutdown_refusal(&self) {
        let mut g = self.child_stdin.lock().unwrap_or_else(|p| p.into_inner());
        g.quitting = g.quitting.map(at_shutdown);
    }

    /// The quit for a retarget, refused with the last retarget's reason read under the
    /// same `desired` lock: a retarget landing between the read and the quit is not lost.
    pub(super) fn send_retarget_quit(&self) -> bool {
        let g = self.desired.lock().unwrap_or_else(|p| p.into_inner());
        self.send_quit(g.2)
    }
}

/// A quit window's refusal once shutdown runs: a retarget's (a game switch, a planned
/// restart) promised an engine that will not come, so "isn't responding". A pause's
/// "Recording is paused" and a stop's "waiting for a game" stay true: they promised none.
pub(super) fn at_shutdown(refusal: Sent) -> Sent {
    match refusal {
        Sent::Switching | Sent::Restarting => Sent::NotResponding,
        other => other,
    }
}

/// A stop says why no engine follows: paused, else waiting for a game.
fn stop_refusal(stop: bool, refusal: Sent) -> Sent {
    match refusal {
        _ if !stop => refusal,
        Sent::Paused => Sent::Paused,
        _ => Sent::Waiting,
    }
}
