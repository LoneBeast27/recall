//! Durable writes taken off the caller's thread or lock, still landing in order
//! (deep-audit round 2, R2-12): each snapshot takes a ticket where it is made (under the
//! owner's lock / `&mut`), and a write never replaces a newer ticket's bytes on disk.
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};

use super::durable;

#[derive(Debug, Default)]
pub struct OrderedWriter {
    next: AtomicU64,
    /// Newest ticket on disk.
    written: Mutex<u64>,
    /// Writes started on a background thread and not finished.
    pending: Mutex<usize>,
    idle: Condvar,
    /// The last background write's error, for the owner to report.
    failed: Mutex<Option<String>>,
}

impl OrderedWriter {
    /// A ticket for the snapshot being made now.
    pub fn ticket(&self) -> u64 {
        self.next.fetch_add(1, Ordering::SeqCst) + 1
    }

    /// Writes `bytes` (temp file, flushed, renamed) unless a newer ticket is already on disk.
    pub fn write(&self, ticket: u64, path: &Path, bytes: &[u8]) -> Result<(), String> {
        let mut written = self.written.lock().unwrap_or_else(|p| p.into_inner());
        if ticket < *written {
            return Ok(());
        }
        durable::write_durable(path, bytes)?;
        *written = ticket;
        Ok(())
    }

    /// [`OrderedWriter::write`] on a short-lived thread; an error is kept for [`take_failure`].
    ///
    /// [`take_failure`]: OrderedWriter::take_failure
    pub fn write_in_background(self: &Arc<Self>, ticket: u64, path: std::path::PathBuf, bytes: Vec<u8>) {
        *self.pending.lock().unwrap_or_else(|p| p.into_inner()) += 1;
        let writer = Arc::clone(self);
        let spawned = std::thread::Builder::new().name("durable-write".into()).spawn(move || {
            let result = writer.write(ticket, &path, &bytes);
            writer.finish(result);
        });
        if let Err(e) = spawned {
            self.finish(Err(format!("start a writer: {e}")));
        }
    }

    fn finish(&self, result: Result<(), String>) {
        if let Err(e) = result {
            *self.failed.lock().unwrap_or_else(|p| p.into_inner()) = Some(e);
        }
        let mut pending = self.pending.lock().unwrap_or_else(|p| p.into_inner());
        *pending = pending.saturating_sub(1);
        self.idle.notify_all();
    }

    /// The error of a background write that failed since the last call.
    pub fn take_failure(&self) -> Option<String> {
        self.failed.lock().unwrap_or_else(|p| p.into_inner()).take()
    }

    /// Blocks until every background write has finished (tests).
    #[cfg(test)]
    pub fn wait(&self) {
        let mut pending = self.pending.lock().unwrap_or_else(|p| p.into_inner());
        while *pending > 0 {
            pending = self.idle.wait(pending).unwrap_or_else(|p| p.into_inner());
        }
    }
}
