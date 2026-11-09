//! The engine's stdin: newline-delimited JSON commands from the controller, applied to
//! shared state on the `recall-stdin` thread (moved out of `main.rs`).
//!
//! The controller always sends `quit` before it closes the pipe, so an end of stdin with
//! no `quit` means the controller is gone (it crashed). The engine is then an orphan whose
//! stdout and stderr lead nowhere: [`reader_ended`] quits it like `quit`, but the save in
//! flight is finished first instead of aborted after 3 s ([`orphaned`]), and every
//! later write to stdout / stderr goes to NUL ([`quiet_std_streams`]) so no `println!` /
//! `eprintln!` panics on the closed pipe.

use std::io::BufRead;
use std::sync::atomic::{AtomicBool, Ordering::Relaxed};
use std::time::Duration;

use crate::protocol;

/// The controller vanished without `quit` (set once, by the stdin thread).
static ORPHANED: AtomicBool = AtomicBool::new(false);
/// How long an orphaned engine lets its saves finish at quit (a save to a slow drive).
pub const ORPHAN_SAVE_BUDGET: Duration = Duration::from_secs(30 * 60);

/// Why the command stream ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReaderEnd {
    /// `{"cmd":"quit"}`.
    Quit,
    /// End of stdin (or a read error) with no `quit`: the controller is gone.
    Eof,
}

/// Read commands until `quit` or the end of the stream; blank, malformed and non-UTF-8
/// lines are skipped (`stdin_commands::read_until_end`).
pub fn read_commands(input: impl BufRead, apply: impl FnMut(protocol::Command)) -> ReaderEnd {
    if crate::stdin_commands::read_until_end(input, apply) {
        ReaderEnd::Quit
    } else {
        ReaderEnd::Eof
    }
}

/// The stream ended: set `quit` (the event loop exits), and on an end with no `quit`
/// mark the engine orphaned (its saves finish before it exits).
pub fn reader_ended(end: ReaderEnd, quit: &AtomicBool, orphaned: &AtomicBool) {
    if end == ReaderEnd::Eof {
        orphaned.store(true, Relaxed);
    }
    quit.store(true, Relaxed);
}

/// The controller vanished: the saver finishes a save in flight at quit instead of
/// aborting it after 3 s (`saver_stop`: only while the save still makes progress, at
/// most [`ORPHAN_SAVE_BUDGET`]).
pub fn orphaned() -> bool {
    ORPHANED.load(Relaxed)
}

/// The engine's standard stream a write failed on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stream {
    Out,
    Err,
}

/// One standard stream pointed at NUL.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Quiet {
    Stdout,
    Stderr,
}

/// What a failed write to `stream` does: (the engine is an orphan, the streams to quiet).
/// Stdout is the protocol: its reader gone means the controller is gone. Stderr is only
/// the log: its reader gone quiets the log alone, the protocol keeps running.
pub fn failure_plan(stream: Stream) -> (bool, &'static [Quiet]) {
    match stream {
        Stream::Out => (true, &[Quiet::Stdout, Quiet::Stderr]),
        Stream::Err => (false, &[Quiet::Stderr]),
    }
}

/// The one line a failed `stream` leaves on the protocol (the controller logs it), so a
/// quieted stderr is not silent: stderr's own failure cannot be written to stderr.
pub fn lost_stream_notice(stream: Stream) -> Option<&'static str> {
    match stream {
        Stream::Err => Some(STDERR_LOST),
        Stream::Out => None,
    }
}

/// [`lost_stream_notice`] for stderr.
pub const STDERR_LOST: &str = "engine log (stderr) lost: its reader is gone; later engine log lines are dropped";

/// A write to `stream` failed: its reader is gone. Later lines on it go to NUL; a
/// broken stdout also marks the engine orphaned (the stdin thread sees the end of the
/// pipe and quits the engine after its saves). A broken stderr says so once on stdout.
pub fn stream_failed(stream: Stream) {
    let (orphan, quiet) = failure_plan(stream);
    if orphan {
        ORPHANED.store(true, Relaxed);
    }
    let mut first = false;
    for which in quiet {
        first |= quiet_std_stream(*which);
    }
    if let Some(message) = lost_stream_notice(stream).filter(|_| first) {
        protocol::emit(&protocol::Outbound::EncoderLog { message: message.into() });
    }
}

/// Point stdout and stderr at NUL: their pipe's reader is gone.
fn quiet_std_streams() {
    quiet_std_stream(Quiet::Stdout);
    quiet_std_stream(Quiet::Stderr);
}

/// Point one standard stream at NUL (once): true the first time.
fn quiet_std_stream(which: Quiet) -> bool {
    use std::os::windows::io::IntoRawHandle;
    static QUIETED: [AtomicBool; 2] = [AtomicBool::new(false), AtomicBool::new(false)];
    #[link(name = "kernel32")]
    extern "system" {
        fn SetStdHandle(which: u32, handle: *mut core::ffi::c_void) -> i32;
    }
    let (slot, handle) = match which {
        Quiet::Stdout => (0, -11i32 as u32), // STD_OUTPUT_HANDLE
        Quiet::Stderr => (1, -12i32 as u32), // STD_ERROR_HANDLE
    };
    if QUIETED[slot].swap(true, Relaxed) {
        return false;
    }
    if let Ok(nul) = std::fs::OpenOptions::new().write(true).open("NUL") {
        // SAFETY: the handle is a fresh, owned NUL handle, handed over for good.
        unsafe { SetStdHandle(handle, nul.into_raw_handle()) };
    }
    true
}

/// Complete the engine's only stdin reader, before any logging to a dead pipe.
pub(crate) fn finish(end: ReaderEnd, quit: &AtomicBool) {
    reader_ended(end, quit, &ORPHANED);
    if end == ReaderEnd::Eof {
        eprintln!("[recall] stdin closed (the controller is gone): quitting after the saves");
        quiet_std_streams();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn a_quit_ends_the_stream_and_the_rest_is_not_read() {
        let mut seen = 0;
        let input = Cursor::new("{\"cmd\":\"bookmark\"}\nnot json\n\n{\"cmd\":\"quit\"}\n{\"cmd\":\"screenshot\"}\n");
        assert_eq!(read_commands(input, |_| seen += 1), ReaderEnd::Quit);
        assert_eq!(seen, 1, "the bookmark only: the malformed line is skipped, nothing after quit");
        let (quit, orphaned) = (AtomicBool::new(false), AtomicBool::new(false));
        reader_ended(ReaderEnd::Quit, &quit, &orphaned);
        assert!(quit.load(Relaxed) && !orphaned.load(Relaxed), "a plain quit keeps the 3 s save budget");
    }

    #[test]
    fn an_end_of_stdin_without_quit_quits_after_the_saves() {
        // R5-overlay-6: the controller crashed; its pipe closed with no `quit`.
        let input = Cursor::new("{\"cmd\":\"bookmark\"}\n");
        assert_eq!(read_commands(input, |_| {}), ReaderEnd::Eof);
        let (quit, orphaned) = (AtomicBool::new(false), AtomicBool::new(false));
        reader_ended(ReaderEnd::Eof, &quit, &orphaned);
        assert!(quit.load(Relaxed), "the orphaned engine quits");
        assert!(orphaned.load(Relaxed), "and lets the save in flight finish");
    }

    #[test]
    fn a_broken_stderr_quiets_the_log_only() {
        // R6-overlay-4: the controller's log reader can stop alone; the protocol on
        // stdout must keep running (no orphan, stdout untouched).
        assert_eq!(failure_plan(Stream::Err), (false, &[Quiet::Stderr][..]));
        assert_eq!(failure_plan(Stream::Out), (true, &[Quiet::Stdout, Quiet::Stderr][..]));
    }

    #[test]
    fn a_quieted_stderr_leaves_one_line_on_the_protocol() {
        // R7-overlay-5: the controller's log shows that the engine's log went quiet.
        assert_eq!(lost_stream_notice(Stream::Err), Some(STDERR_LOST));
        assert_eq!(lost_stream_notice(Stream::Out), None, "a broken stdout has no protocol to say it on");
        let line = serde_json::to_value(protocol::Outbound::EncoderLog { message: STDERR_LOST.into() }).unwrap();
        assert_eq!(line["type"], "encoder_log", "the controller logs encoder_log lines");
    }
}
