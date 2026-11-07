//! Two serial job threads instead of one: library work (listings, deletes) runs apart from the
//! editor's io (saves, flushes, exports, storage), so a long export never holds up a delete or
//! a newly added folder. Each thread keeps its own jobs in order. A delete still waits for the
//! saves and flushes queued before it for the same clips ([`PathGate`]), so a sidecar is never
//! written beside a clip that already went to the Recycle Bin.
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

/// Clips with an io job (save, flush) queued or running, by clip key.
#[derive(Clone, Default)]
pub struct PathGate {
    inner: Arc<(Mutex<HashMap<String, usize>>, Condvar)>,
}

/// Longest a delete waits for a clip's flush (a flush stuck behind a long export).
const GATE_WAIT: Duration = Duration::from_secs(600);

impl PathGate {
    pub fn enter(&self, key: String) {
        let (lock, _) = &*self.inner;
        *lock.lock().unwrap_or_else(|p| p.into_inner()).entry(key).or_default() += 1;
    }

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
}

#[cfg(test)]
mod tests {
    use super::*;

    enum Fake {
        Export(mpsc::Receiver<()>),
        Delete,
    }

    fn route(job: &Fake) -> Lane {
        match job {
            Fake::Export(_) => Lane::Io,
            Fake::Delete => Lane::Library,
        }
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
