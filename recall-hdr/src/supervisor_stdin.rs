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
use std::sync::{Arc, Mutex};
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
    /// The engine's whole latest audio state (the cut-down): one slot, replaced by every newer
    /// state, never dropped. The writer writes it on its wake line, before a `quit`, and again
    /// after a press line (so the last thing the engine reads is always the newest state).
    state: Arc<Mutex<StateSlot>>,
}

#[derive(Default)]
struct StateSlot {
    lines: Vec<String>,
    /// Not written since it was set.
    fresh: bool,
}

/// The line that wakes the writer for a new state (never written to the engine).
const WAKE: &str = "";

/// The `quit` line (`Shared::send_quit`).
pub(super) const QUIT_LINE: &str = r#"{"cmd":"quit"}"#;

impl StdinQueue {
    /// Start the writer thread for `out` (the child's stdin).
    pub fn start(out: Box<dyn Write + Send>) -> StdinQueue {
        let (tx, rx) = sync_channel::<String>(QUEUE + 1);
        let waiting = Arc::new(AtomicUsize::new(0));
        let taken = waiting.clone();
        let state = Arc::new(Mutex::new(StateSlot::default()));
        let slot = state.clone();
        let spawned = std::thread::Builder::new().name("recall-engine-stdin".into()).spawn(move || {
            let mut out = out;
            for line in rx {
                let _ = taken.fetch_update(Ordering::SeqCst, Ordering::SeqCst, |n| n.checked_sub(1));
                let press = line.contains(r#""cmd":"set_mic_mute""#);
                let mut lines = Vec::new();
                if line == WAKE || line == QUIT_LINE {
                    lines.extend(take_state(&slot, false)); // the newest state, ahead of a quit
                }
                if line != WAKE {
                    lines.push(line);
                }
                // After any other line, a newer state if one is waiting (its wake may have been
                // full); after a press, the state again so the engine ends on it.
                if press || !lines.is_empty() {
                    lines.extend(take_state(&slot, press));
                }
                let quit = lines.iter().any(|l| l == QUIT_LINE);
                if !lines.into_iter().all(|l| write_line(&mut out, l)) || quit {
                    break; // the engine is gone (or killed), or reads nothing after its quit
                }
            }
        });
        if let Err(e) = spawned {
            log!("supervisor: engine stdin writer failed to start: {e}");
        }
        StdinQueue { tx, waiting, dropped: AtomicU64::new(0), state }
    }

    /// The engine's whole latest audio state: it replaces any state not yet written (the cut-down).
    /// Never dropped: when the queue is full the wake is skipped and the writer writes the
    /// state after the line it is on.
    pub fn send_state(&self, lines: Vec<String>) {
        *self.state.lock().unwrap_or_else(|p| p.into_inner()) = StateSlot { lines, fresh: true };
        if self.waiting.fetch_add(1, Ordering::SeqCst) >= QUEUE || self.tx.try_send(WAKE.to_owned()).is_err() {
            self.waiting.fetch_sub(1, Ordering::SeqCst); // full: the state rides after the next line
        }
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

/// The state's lines when fresh (or always, `again`), marked written.
fn take_state(slot: &Mutex<StateSlot>, again: bool) -> Vec<String> {
    let mut s = slot.lock().unwrap_or_else(|p| p.into_inner());
    if !(s.fresh || again) {
        return Vec::new();
    }
    s.fresh = false;
    s.lines.clone()
}

/// One line and its newline in one write: the engine never sees half a line (merge-gate P1-2).
fn write_line(out: &mut Box<dyn Write + Send>, line: String) -> bool {
    let mut bytes = line.into_bytes();
    bytes.push(b'\n');
    out.write_all(&bytes).and_then(|_| out.flush()).is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    /// Each `write` call the engine's pipe would get, as one chunk.
    struct Chunks(Arc<Mutex<Vec<Vec<u8>>>>);

    impl Write for Chunks {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().push(buf.to_vec());
            Ok(buf.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    /// Merge-gate P1-2: the text and its newline went out as two writes, so the engine could
    /// see an empty pipe mid-line and confirm its drain before the line was whole.
    #[test]
    fn a_line_and_its_newline_go_out_in_one_write() {
        let chunks = Arc::new(Mutex::new(Vec::new()));
        let queue = StdinQueue::start(Box::new(Chunks(chunks.clone())));
        let line = r#"{"cmd":"set_mic_mute","muted":true}"#;
        assert!(queue.send(line));
        drop(queue); // the writer ends once the queue is closed
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while chunks.lock().unwrap().is_empty() && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        std::thread::sleep(std::time::Duration::from_millis(50)); // a second write, if any
        assert_eq!(*chunks.lock().unwrap(), vec![format!("{line}\n").into_bytes()], "one write, newline included");
    }

    /// A writer stuck on its first line (released by the test).
    struct Slow(Arc<Mutex<Vec<Vec<u8>>>>, Arc<Mutex<()>>);

    impl Write for Slow {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            let _wait = self.1.lock().unwrap();
            self.0.lock().unwrap().push(buf.to_vec());
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    fn written(chunks: &Arc<Mutex<Vec<Vec<u8>>>>, at_least: usize) -> Vec<String> {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while chunks.lock().unwrap().len() < at_least && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
        chunks.lock().unwrap().iter().map(|c| String::from_utf8_lossy(c).trim().to_owned()).collect()
    }

    /// The cut-down: with the engine not reading and the queue full, every audio state still
    /// gets through: the newest replaces the older ones (never two old ones, never none), and it
    /// is written ahead of a queued `quit`.
    #[test]
    fn the_state_slot_coalesces_is_never_dropped_and_goes_ahead_of_the_quit() {
        let chunks = Arc::new(Mutex::new(Vec::new()));
        let gate = Arc::new(Mutex::new(()));
        let held = gate.lock().unwrap();
        let queue = StdinQueue::start(Box::new(Slow(chunks.clone(), gate.clone())));
        assert!(queue.send(r#"{"cmd":"bookmark"}"#));
        std::thread::sleep(std::time::Duration::from_millis(50)); // the writer is stuck on it
        while queue.send(r#"{"cmd":"bookmark"}"#) {}
        queue.send_state(vec![r#"{"cmd":"set_mic_mute","muted":false}"#.into()]);
        queue.send_state(vec![r#"{"cmd":"set_mic_mute","muted":true}"#.into(), r#"{"cmd":"sync"}"#.into()]);
        assert!(queue.send_quit(QUIT_LINE));
        drop(held);
        drop(queue);
        let lines = written(&chunks, QUEUE + 3);
        assert!(!lines.iter().any(|l| l.contains(r#""muted":false"#)), "the older state is replaced: {lines:?}");
        let state = lines.iter().position(|l| l.contains(r#""muted":true"#)).expect("the newest state is written");
        let quit = lines.iter().position(|l| l == QUIT_LINE).expect("the quit");
        assert!(state < quit, "ahead of the quit");
        assert_eq!(lines[state + 1], r#"{"cmd":"sync"}"#);
    }

    /// After a press line the writer writes the newest state again, so a stale press queued
    /// before a newer state never ends the engine on the wrong mute.
    #[test]
    fn a_press_is_followed_by_the_newest_state() {
        let chunks = Arc::new(Mutex::new(Vec::new()));
        let gate = Arc::new(Mutex::new(()));
        let held = gate.lock().unwrap();
        let queue = StdinQueue::start(Box::new(Slow(chunks.clone(), gate.clone())));
        assert!(queue.send(r#"{"cmd":"bookmark"}"#));
        std::thread::sleep(std::time::Duration::from_millis(50));
        assert!(queue.send(r#"{"cmd":"set_mic_mute","muted":true,"press":true}"#)); // the press
        queue.send_state(vec![r#"{"cmd":"set_mic_mute","muted":false}"#.into()]); // then unmuted
        drop(held);
        drop(queue);
        let lines = written(&chunks, 3);
        assert_eq!(lines.last().map(String::as_str), Some(r#"{"cmd":"set_mic_mute","muted":false}"#), "{lines:?}");
    }
}
