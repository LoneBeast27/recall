//! The engine's stdin, written on its own thread so no caller ever blocks on the pipe.
//!
//! A hung engine stops reading its stdin; once the pipe fills, a write blocks for good. The
//! pump thread (hotkeys, `record_fail`) and the manager (the `quit` before a kill) only put
//! lines on a bounded queue ([`QUEUE`]) and return at once; the `recall-engine-stdin`
//! thread does the writing. One slot beyond [`QUEUE`] is kept for `quit`, so a healthy
//! engine that is only slow still gets its clean finish behind a full queue. A full queue
//! drops the line and says so (`send` is false; every user action then shows "Recorder
//! isn't responding", `record_dispatch.rs`), logged and counted: the engine is
//! not reading, so the manager's kill after its grace is what ends it, and a long
//! recording a dropped `record_fail` could not end is recovered from its `.part` on the
//! next start. The writer thread ends when the queue is closed (the child is replaced) or
//! a write fails (the kill breaks the pipe).

use std::io::Write;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;
use std::sync::mpsc::{sync_channel, SyncSender, TrySendError};

use crate::log;

/// Lines that may wait for the engine to read them.
pub(super) const QUEUE: usize = 64;

/// The queue to one engine's stdin writer.
pub(super) struct StdinQueue {
    tx: SyncSender<String>,
    /// Lines queued and not yet taken by the writer (the reserved quit slot excluded).
    waiting: Arc<AtomicUsize>,
    dropped: AtomicU64,
}

impl StdinQueue {
    /// Start the writer thread for `out` (the child's stdin).
    pub fn start(out: Box<dyn Write + Send>) -> StdinQueue {
        let (tx, rx) = sync_channel::<String>(QUEUE + 1);
        let waiting = Arc::new(AtomicUsize::new(0));
        let taken = waiting.clone();
        let spawned = std::thread::Builder::new().name("recall-engine-stdin".into()).spawn(move || {
            let mut out = out;
            for line in rx {
                let _ = taken.fetch_update(Ordering::SeqCst, Ordering::SeqCst, |n| n.checked_sub(1));
                let wrote = out.write_all(line.as_bytes()).and_then(|_| out.write_all(b"\n")).and_then(|_| out.flush());
                if wrote.is_err() {
                    break; // the engine is gone (or killed): nothing more reaches it
                }
            }
        });
        if let Err(e) = spawned {
            log!("supervisor: engine stdin writer failed to start: {e}");
        }
        StdinQueue { tx, waiting, dropped: AtomicU64::new(0) }
    }

    /// `quit`: may take the slot kept beyond [`QUEUE`]; never blocks.
    pub fn send_quit(&self, line: &str) -> bool {
        self.tx.try_send(line.to_string()).is_ok()
    }

    /// Queue one line; never blocks. False when it was dropped (queue full or writer gone).
    pub fn send(&self, line: &str) -> bool {
        // Count first, so two callers can never both take the last ordinary slot.
        if self.waiting.fetch_add(1, Ordering::SeqCst) >= QUEUE {
            self.waiting.fetch_sub(1, Ordering::SeqCst);
            return self.dropped(line);
        }
        match self.tx.try_send(line.to_string()) {
            Ok(()) => true,
            Err(e) => {
                self.waiting.fetch_sub(1, Ordering::SeqCst);
                match e {
                    TrySendError::Full(_) => self.dropped(line),
                    TrySendError::Disconnected(_) => false,
                }
            }
        }
    }

    fn dropped(&self, line: &str) -> bool {
        let n = self.dropped.fetch_add(1, Ordering::Relaxed) + 1;
        if n == 1 || n.is_multiple_of(100) {
            log!("supervisor: engine is not reading its stdin; dropped {n} line(s) (now {line})");
        }
        false
    }
}
