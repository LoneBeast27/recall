//! F1: the stop sequence's waits after the kill are bounded, with a process that never ends
//! (a thread stuck in a driver wait) standing in for the engine.

use super::*;
use crate::record_dispatch::Sent;
use std::sync::Arc;

/// A process the kill cannot end (or, with `ends_on_kill`, one that ends at once).
struct Unkillable {
    killed: bool,
    ends_on_kill: bool,
}

impl Reap for Unkillable {
    fn id(&self) -> u32 {
        4242
    }
    fn try_wait(&mut self) -> io::Result<Option<ExitStatus>> {
        use std::os::windows::process::ExitStatusExt;
        Ok((self.killed && self.ends_on_kill).then(|| ExitStatus::from_raw(1)))
    }
    fn kill(&mut self) -> io::Result<()> {
        self.killed = true;
        Ok(())
    }
}

#[test]
fn the_wait_after_the_kill_is_bounded() {
    let mut stuck = Unkillable { killed: false, ends_on_kill: false };
    let started = Instant::now();
    assert!(!kill_bounded(&mut stuck, Duration::from_millis(300)), "it never ended");
    assert!(stuck.killed && started.elapsed() < Duration::from_secs(2), "{:?}", started.elapsed());
    assert!(kill_bounded(&mut Unkillable { killed: false, ends_on_kill: true }, Duration::from_secs(5)), "a normal kill");
}

#[test]
fn the_join_of_a_reader_whose_pipe_never_closes_is_bounded() {
    let (_tx, rx) = std::sync::mpsc::channel::<()>();
    let reader = thread::spawn(move || {
        let _ = rx.recv(); // the stdout of a process that never ends
    });
    let started = Instant::now();
    assert!(!join_bounded(reader, Duration::from_millis(300)));
    assert!(started.elapsed() < Duration::from_secs(2));
    assert!(join_bounded(thread::spawn(|| {}), Duration::from_secs(5)));
}

/// The live stop sequence (`finish_shutdown`, the grace run out) with an engine the kill cannot
/// end: it returns within KILL_WAIT and hands the engine back; abandoning it tells the
/// controller, and a slot-busy spawn while it still runs tells it once more.
#[test]
fn a_stuck_engine_is_handed_back_and_said_and_the_slot_busy_spawn_says_it_again() {
    let shared = Arc::new(Shared::new(None));
    let (tx, events) = std::sync::mpsc::channel();
    *shared.events.lock().unwrap() = Some(tx);
    let waiter = shared.clone();
    let (done_tx, done) = std::sync::mpsc::channel();
    thread::spawn(move || {
        let left = super::super::manager::finish_shutdown(&waiter, Box::new(Unkillable { killed: false, ends_on_kill: false }), Duration::from_millis(100), Some(Sent::NotResponding), false);
        let _ = done_tx.send(left);
    });
    let left = done.recv_timeout(KILL_WAIT + Duration::from_secs(3)).expect("the stop sequence returned (it waited for ever)");
    let left = left.expect("the stuck engine is handed back");
    shared.abandon(left);
    let kinds = |rx: &std::sync::mpsc::Receiver<serde_json::Value>| rx.try_iter().filter(|e| e["type"] == ENGINE_STUCK).count();
    assert_eq!(kinds(&events), 1, "the controller is told");
    shared.slot_held_by_stuck();
    shared.slot_held_by_stuck();
    assert_eq!(kinds(&events), 1, "the next engine found the slot held: told once more, not every retry");
}
