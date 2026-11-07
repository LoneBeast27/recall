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
use std::sync::Arc;
use std::time::Duration;

use crate::capture::CaptureShared;
use crate::{engine_extras, protocol, ExportHook, SaveHook};

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

/// A write to `stream` failed: its reader is gone. Later lines on it go to NUL; a
/// broken stdout also marks the engine orphaned (the stdin thread sees the end of the
/// pipe and quits the engine after its saves).
pub fn stream_failed(stream: Stream) {
    let (orphan, quiet) = failure_plan(stream);
    if orphan {
        ORPHANED.store(true, Relaxed);
    }
    for which in quiet {
        quiet_std_stream(*which);
    }
}

/// Point stdout and stderr at NUL: their pipe's reader is gone.
fn quiet_std_streams() {
    quiet_std_stream(Quiet::Stdout);
    quiet_std_stream(Quiet::Stderr);
}

/// Point one standard stream at NUL (once).
fn quiet_std_stream(which: Quiet) {
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
        return;
    }
    if let Ok(nul) = std::fs::OpenOptions::new().write(true).open("NUL") {
        // SAFETY: the handle is a fresh, owned NUL handle, handed over for good.
        unsafe { SetStdHandle(handle, nul.into_raw_handle()) };
    }
}

/// Start the stdin thread. `save` triggers a replay-buffer save (None when encode is off).
pub(crate) fn spawn(shared: Arc<CaptureShared>, save: Option<SaveHook>, export: Option<ExportHook>, extras: engine_extras::Extras) {
    std::thread::spawn(move || {
        let stdin = std::io::stdin();
        let end = read_commands(stdin.lock(), |cmd| apply(cmd, &shared, &save, &export, &extras));
        reader_ended(end, &shared.quit, &ORPHANED);
        if end == ReaderEnd::Eof {
            // Quit first (above): with the controller gone this line may go nowhere.
            eprintln!("[recall] stdin closed (the controller is gone): quitting after the saves");
            quiet_std_streams();
        }
    });
}

fn apply(cmd: protocol::Command, shared: &CaptureShared, save: &Option<SaveHook>, export: &Option<ExportHook>, extras: &engine_extras::Extras) {
    match cmd {
        protocol::Command::SetMode { value } => shared.set_select(&value),
        protocol::Command::SaveClip { seconds, at_last_frame } => match save {
            Some(hook) => hook(seconds, at_last_frame),
            None => protocol::emit(&protocol::Outbound::SaveFailed {
                reason: "engine built without the encode feature".to_string(),
                rejected: true,
            }),
        },
        protocol::Command::ExportClip { path, output, fps, edit } => match export {
            Some(hook) => hook(path, output, fps, edit),
            None => protocol::emit(&protocol::Outbound::ExportFailed {
                source: path,
                reason: "engine built without export support".into(),
            }),
        },
        protocol::Command::ToggleMicMute => {
            #[cfg(feature = "encode")]
            protocol::emit(&protocol::Outbound::MicMuted { muted: crate::audio::toggle_mic_mute() });
        }
        protocol::Command::SetMicMute { muted } => {
            #[cfg(feature = "encode")]
            {
                crate::audio::set_mic_muted(muted);
                protocol::emit(&protocol::Outbound::MicMuted { muted });
            }
            #[cfg(not(feature = "encode"))]
            let _ = muted;
        }
        protocol::Command::SetSource { id, gain_db, name } => {
            #[cfg(feature = "encode")]
            {
                if !crate::audio::sources::set_live(&id, gain_db, name.as_deref()) {
                    eprintln!("[recall] set_source: no live track '{id}'");
                }
            }
            #[cfg(not(feature = "encode"))]
            let _ = (id, gain_db, name);
        }
        cmd @ (protocol::Command::Bookmark
        | protocol::Command::Screenshot
        | protocol::Command::HudProbe
        | protocol::Command::RecordStart
        | protocol::Command::RecordStop
        | protocol::Command::RecordToggle
        | protocol::Command::RecordFail { .. }) => {
            extras.handle(&cmd);
        }
        protocol::Command::Quit => {} // ends the stream in `read_commands`
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
}
