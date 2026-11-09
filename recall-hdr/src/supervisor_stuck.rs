//! An engine that does not end after the kill (fresh review F1). `TerminateProcess` cannot
//! finish while a thread of the engine sits in a non-cancellable driver wait (a dying USB, HDD
//! or network drive): an unbounded `child.wait()` then held the manager thread for ever, so the
//! next game was never recorded, silently, and the app's quit (which joins the manager) hung.
//!
//! Now the wait after the kill and the join of the stdout reader are bounded ([`KILL_WAIT`],
//! [`READER_WAIT`]). Past them the engine is left running (its handle kept), the controller is
//! told ([`ENGINE_STUCK`]: "The recorder is stuck" / "Restart the PC if recording doesn't
//! start"), its exit is forwarded as usual (the saves it had are reported lost) and the manager
//! goes on. The stuck engine may still hold the engine slot (`Local\Recall.Engine`): the next
//! engine then exits with the slot-busy code, and the first such exit while the stuck one still
//! runs repeats the notice ([`Shared::slot_held_by_stuck`]), so the next game never fails
//! silently.

use std::io;
use std::process::{Child, ExitStatus};
use std::sync::Mutex;
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use super::{Shared, SHUTDOWN_POLL};
use crate::log;

/// How long the manager waits for the process to end after the kill.
pub(super) const KILL_WAIT: Duration = Duration::from_secs(5);
/// How long it waits for the engine's stdout reader after that.
pub(super) const READER_WAIT: Duration = Duration::from_secs(5);
/// The event the controller turns into the notice below.
pub const ENGINE_STUCK: &str = "engine_stuck";
pub const STUCK_TITLE: &str = "The recorder is stuck";
pub const STUCK_DETAIL: &str = "Restart the PC if recording doesn't start";

/// The engine process as the stop sequence drives it (tests: a process that never ends).
pub(super) trait Reap: Send {
    fn id(&self) -> u32;
    fn try_wait(&mut self) -> io::Result<Option<ExitStatus>>;
    fn kill(&mut self) -> io::Result<()>;
}

impl Reap for Child {
    fn id(&self) -> u32 {
        Child::id(self)
    }
    fn try_wait(&mut self) -> io::Result<Option<ExitStatus>> {
        Child::try_wait(self)
    }
    fn kill(&mut self) -> io::Result<()> {
        Child::kill(self)
    }
}

/// Kill and wait at most `bound` for the process to end: false = it is still there.
pub(super) fn kill_bounded(child: &mut dyn Reap, bound: Duration) -> bool {
    let _ = child.kill();
    let until = Instant::now() + bound;
    loop {
        match child.try_wait() {
            Ok(None) if Instant::now() < until => thread::sleep(SHUTDOWN_POLL),
            Ok(None) => return false,
            Ok(Some(_)) | Err(_) => return true,
        }
    }
}

/// Join `reader` within `bound`; false = it is left running (the pipe never closed).
pub(super) fn join_bounded(reader: JoinHandle<()>, bound: Duration) -> bool {
    let until = Instant::now() + bound;
    while !reader.is_finished() {
        if Instant::now() >= until {
            return false;
        }
        thread::sleep(SHUTDOWN_POLL);
    }
    let _ = reader.join();
    true
}

/// The engine left running after its kill, and whether a slot-busy spawn repeated the notice.
#[derive(Default)]
pub(super) struct Stuck(Mutex<Option<(Box<dyn Reap>, bool)>>);

impl Shared {
    /// Keep the engine that did not end and tell the controller.
    pub(super) fn abandon(&self, child: Box<dyn Reap>) {
        log!("supervisor: engine (pid {}) did not end {} s after the kill (stuck in a driver); left running, the next engine starts", child.id(), KILL_WAIT.as_secs());
        *self.stuck.0.lock().unwrap_or_else(|p| p.into_inner()) = Some((child, false));
        self.forward_stuck();
    }

    /// A spawn found the engine slot held: while our stuck engine still runs, say so once more.
    pub(super) fn slot_held_by_stuck(&self) {
        let mut stuck = self.stuck.0.lock().unwrap_or_else(|p| p.into_inner());
        let Some((child, told)) = stuck.as_mut() else { return };
        if !matches!(child.try_wait(), Ok(None)) {
            *stuck = None; // it ended after all
            return;
        }
        if !std::mem::replace(told, true) {
            drop(stuck);
            self.forward_stuck();
        }
    }

    fn forward_stuck(&self) {
        if let Ok(g) = self.events.lock() {
            if let Some(tx) = g.as_ref() {
                let _ = tx.send(serde_json::json!({ "type": ENGINE_STUCK }));
            }
        }
    }
}

#[cfg(test)]
#[path = "supervisor_stuck_tests.rs"]
mod tests;
