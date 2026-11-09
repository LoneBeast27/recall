//! Two serial job threads instead of one: library work (listings, deletes) runs apart from the
//! editor's io (saves, flushes, exports, storage), so a long export never holds up a delete or
//! a newly added folder. Each thread keeps its own jobs in order. A delete still waits for the
//! saves and flushes queued before it for the same clips ([`PathGate`]), so a sidecar is never
//! written beside a clip that already went to the Recycle Bin. That wait happens on a thread of
//! its own ([`PathGate::run_when_clear`]): listings keep running on the library thread.
use std::{
    collections::HashMap,
    sync::{
        Arc, Condvar, Mutex,
        mpsc::{self, Sender},
    },
    thread,
    time::Duration,
};

/// Which thread a job runs on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lane {
    /// Saves, flushes, exports, drive space, storage reports.
    Io,
    /// Listings and deletes.
    Library,
}

pub struct Lanes<J> {
    io: Sender<J>,
    library: Sender<J>,
    route: fn(&J) -> Lane,
}

impl<J: Send + 'static> Lanes<J> {
    pub fn inert(route: fn(&J) -> Lane) -> Self {
        Self { io: mpsc::channel().0, library: mpsc::channel().0, route }
    }

    /// Starts both threads; `io` and `library` each run their lane's jobs one at a time.
    pub fn spawn(route: fn(&J) -> Lane, io: impl FnMut(J) -> bool + Send + 'static, library: impl FnMut(J) -> bool + Send + 'static) -> Self {
        Self { io: start("recall-io", io), library: start("recall-library", library), route }
    }

    pub fn send(&self, job: J) {
        let lane = match (self.route)(&job) {
            Lane::Io => &self.io,
            Lane::Library => &self.library,
        };
        let _ = lane.send(job);
    }
}

/// A thread that runs jobs until the channel closes or `run` says stop (its receiver is gone).
fn start<J: Send + 'static>(name: &str, mut run: impl FnMut(J) -> bool + Send + 'static) -> Sender<J> {
    let (tx, rx) = mpsc::channel::<J>();
    let spawned = thread::Builder::new().name(name.into()).spawn(move || {
        while let Ok(job) = rx.recv() {
            if !run(job) {
                break;
            }
        }
    });
    if let Err(error) = spawned {
        eprintln!("start {name}: {error}");
    }
    tx
}

/// Clips with an edit job (save, flush) queued or running, by clip key (`library::clip_key`).
///
/// The contract, for every thread that runs such jobs (today the io lane; an edits lane later):
/// - [`PathGate::enter`] once per job, on the sending side, *before* the job is sent
///   (`Workers::job` does it), so a delete sent afterwards always sees it;
/// - [`PathGate::leave`] once per job, on the running side, *after* the job has finished
///   writing, whether it succeeded or not. A missing `leave` holds every delete of that clip for
///   [`GATE_WAIT`]; a missing `enter` lets a delete overtake the save.
#[derive(Clone, Default)]
pub struct PathGate {
    inner: Arc<(Mutex<HashMap<String, usize>>, Condvar)>,
}

/// Longest a delete waits for a clip's flush (a flush stuck behind a long export). After it the
/// delete runs even if the flush is still blocked on an unresponsive drive (review RI-9): the
/// clip goes to the Recycle Bin and the late sidecar write may leave a `.recall-edit.json`
/// beside where it was. No clip is lost; the orphan sidecar is harmless and small. Raising the
/// limit would only hold every delete of that clip longer.
const GATE_WAIT: Duration = Duration::from_secs(600);

impl PathGate {
    /// A save or flush of `key` was queued (call before sending it).
    pub fn enter(&self, key: String) {
        let (lock, _) = &*self.inner;
        *lock.lock().unwrap_or_else(|p| p.into_inner()).entry(key).or_default() += 1;
    }

    /// That save or flush of `key` has finished (call after it ran, success or not).
    pub fn leave(&self, key: &str) {
        let (lock, ready) = &*self.inner;
        let mut pending = lock.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(count) = pending.get_mut(key) {
            *count -= 1;
            if *count == 0 {
                pending.remove(key);
            }
        }
        ready.notify_all();
    }

    /// Blocks until none of `keys` has an io job pending (or [`GATE_WAIT`] passed).
    pub fn wait_clear(&self, keys: &[String]) {
        let (lock, ready) = &*self.inner;
        let pending = lock.lock().unwrap_or_else(|p| p.into_inner());
        let _ = ready.wait_timeout_while(pending, GATE_WAIT, |pending| keys.iter().any(|key| pending.contains_key(key)));
    }

    /// True when none of `keys` has a save or flush pending.
    pub fn is_clear(&self, keys: &[String]) -> bool {
        let (lock, _) = &*self.inner;
        let pending = lock.lock().unwrap_or_else(|p| p.into_inner());
        !keys.iter().any(|key| pending.contains_key(key))
    }

    /// Runs `job` now when none of `keys` is pending. Otherwise calls `waiting` and runs `job`
    /// on a thread of its own once they clear (or [`GATE_WAIT`] passed), so the calling lane
    /// keeps serving its other jobs. True when it had to wait.
    pub fn run_when_clear(&self, keys: Vec<String>, waiting: impl FnOnce(), job: impl FnOnce() + Send + 'static) -> bool {
        if self.is_clear(&keys) {
            job();
            return false;
        }
        waiting();
        let slot = Arc::new(Mutex::new(Some((keys, job))));
        let (gate, theirs) = (self.clone(), slot.clone());
        let spawned = thread::Builder::new().name("recall-delete-wait".into()).spawn(move || {
            if let Some((keys, job)) = theirs.lock().unwrap_or_else(|p| p.into_inner()).take() {
                gate.wait_clear(&keys);
                job();
            }
        });
        if let Err(error) = spawned {
            // No thread: wait here rather than drop the job.
            eprintln!("start recall-delete-wait: {error}");
            if let Some((keys, job)) = slot.lock().unwrap_or_else(|p| p.into_inner()).take() {
                self.wait_clear(&keys);
                job();
            }
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    enum Fake {
        Export(mpsc::Receiver<()>),
        Delete,
        List,
    }

    fn route(job: &Fake) -> Lane {
        match job {
            Fake::Export(_) => Lane::Io,
            Fake::Delete | Fake::List => Lane::Library,
        }
    }

    #[test]
    fn a_delete_waiting_for_a_flush_does_not_hold_up_listings() {
        let gate = PathGate::default();
        gate.enter("a.mp4".into()); // the flush is queued behind an export
        let (tx, done) = mpsc::channel::<&'static str>();
        let lib_gate = gate.clone();
        let lanes = Lanes::spawn(
            route,
            |_| true,
            move |job| {
                match job {
                    Fake::Delete => {
                        let (waiting, deleted) = (tx.clone(), tx.clone());
                        lib_gate.run_when_clear(
                            vec!["a.mp4".into()],
                            move || {
                                let _ = waiting.send("waiting");
                            },
                            move || {
                                let _ = deleted.send("deleted");
                            },
                        );
                    }
                    _ => {
                        let _ = tx.send("listed");
                    }
                }
                true
            },
        );
        lanes.send(Fake::Delete);
        lanes.send(Fake::List);
        assert_eq!(done.recv_timeout(Duration::from_secs(1)), Ok("waiting"), "the dialog can say why it waits");
        assert_eq!(done.recv_timeout(Duration::from_secs(1)), Ok("listed"), "a listing answers while the delete waits");
        assert!(done.recv_timeout(Duration::from_millis(200)).is_err(), "the delete still waits for the flush");
        gate.leave("a.mp4");
        assert_eq!(done.recv_timeout(Duration::from_secs(5)), Ok("deleted"));
        // Nothing pending: the delete runs at once, in order.
        assert!(!gate.run_when_clear(vec!["a.mp4".into()], || panic!("no wait"), || {}));
    }

    #[test]
    fn a_delete_does_not_wait_for_a_running_export() {
        let (done_tx, done) = mpsc::channel::<&'static str>();
        let (io_tx, lib_tx) = (done_tx.clone(), done_tx);
        let lanes = Lanes::spawn(
            route,
            move |job| {
                if let Fake::Export(release) = job {
                    let _ = release.recv_timeout(Duration::from_secs(10));
                    let _ = io_tx.send("exported");
                }
                true
            },
            move |_| lib_tx.send("deleted").is_ok(),
        );
        let (release, held) = mpsc::channel();
        lanes.send(Fake::Export(held));
        lanes.send(Fake::Delete);
        assert_eq!(done.recv_timeout(Duration::from_secs(5)), Ok("deleted"), "the delete answers while the export runs");
        release.send(()).unwrap();
        assert_eq!(done.recv_timeout(Duration::from_secs(5)), Ok("exported"));
    }

    #[test]
    fn the_gate_holds_only_the_clips_with_a_pending_flush() {
        let gate = PathGate::default();
        gate.enter("a.mp4".into());
        gate.wait_clear(&["b.mp4".into()]); // another clip: no wait
        let (tx, rx) = mpsc::channel();
        let waiter = gate.clone();
        thread::spawn(move || {
            waiter.wait_clear(&["a.mp4".into()]);
            let _ = tx.send(());
        });
        assert!(rx.recv_timeout(Duration::from_millis(200)).is_err(), "waits while the flush is pending");
        gate.leave("a.mp4");
        assert!(rx.recv_timeout(Duration::from_secs(5)).is_ok());
    }
}
