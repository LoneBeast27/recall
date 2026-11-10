//! The engine's stdin, split out of `main.rs`. It is read from before any audio track opens
//! (merge gate p9, mute race): an Alt+M the controller sends right after the spawn applies at
//! once (`audio::set_mic_muted`, read by every packet), not after every track has started. The
//! other commands wait in a channel until the encoder and hooks exist ([`serve`]).
//!
//! R2-rel-1: the end of the stream (`quit`, `stop_capture`, end of stdin) goes down the same
//! channel, so `quit` is set by the serve thread only after it applied every command read
//! before it: a save pressed just before a privacy stop reaches the encoder even when the
//! serve thread is starved (a game at full CPU), instead of being sent to an encoder that
//! already drained and exited. `stop_capture` itself still stops capture at once, on the
//! stdin thread. If the serve thread does not take the end within [`SERVE_TAKES_END`] (it
//! never started, or is stuck in a hook) the stdin thread ends it itself, so a quit is never
//! held longer than that.

use std::io::BufRead;
use std::sync::atomic::{AtomicBool, Ordering::SeqCst};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::Arc;
use std::time::Duration;

use crate::capture::CaptureShared;
use crate::protocol::{self, Command};
use crate::stdin_reader::ReaderEnd;
use crate::{engine_extras, stdin_reader, ExportHook, SaveHook};
#[cfg(feature = "encode")]
use crate::audio;

/// The longest the stdin thread waits for the serve thread to act on the end of the stream.
pub(crate) const SERVE_TAKES_END: Duration = Duration::from_secs(2);

/// What the stdin thread hands the serve thread, in the order it was read.
pub(crate) enum Item {
    Cmd(Command),
    /// The stream ended: set `quit` (and the orphan flag) now, after every command before it.
    End(ReaderEnd),
}

/// Commands read so far, for [`serve`].
pub(crate) struct Early {
    rx: Receiver<Item>,
    ends: Ender,
}

/// Acts on the end of the stream once, from whichever thread gets there first.
#[derive(Clone)]
struct Ender {
    shared: Arc<CaptureShared>,
    done: Arc<AtomicBool>,
    taken: Sender<()>,
}

impl Ender {
    fn end(&self, end: ReaderEnd) {
        if !self.done.swap(true, SeqCst) {
            stdin_reader::finish(end, &self.shared.quit);
        }
    }
}

/// Start reading the process's stdin now; `quit` is set after a `quit` line or the end of stdin.
pub(crate) fn spawn_early(shared: Arc<CaptureShared>) -> Early {
    spawn_early_from(|| std::io::stdin().lock(), shared)
}

/// [`spawn_early`] from the reader `open` gives (tests: a pipe).
pub(crate) fn spawn_early_from<R: BufRead>(open: impl FnOnce() -> R + Send + 'static, shared: Arc<CaptureShared>) -> Early {
    let (tx, rx) = channel();
    let (taken, taken_rx) = channel();
    let ends = Ender { shared, done: Arc::new(AtomicBool::new(false)), taken };
    let mine = ends.clone();
    std::thread::spawn(move || {
        let forward = tx.clone();
        let end = read_early(open(), |cmd| {
            let _ = forward.send(Item::Cmd(cmd));
        });
        if end == ReaderEnd::StopCapture {
            crate::stop_capture::request(); // at once: not one more frame or sample (F-detect-1)
        }
        let _ = tx.send(Item::End(end));
        // R2-rel-1: the serve thread sets `quit` after the commands before it; only if it does
        // not within the bound does this thread (a quit never waits longer than that).
        let _ = taken_rx.recv_timeout(SERVE_TAKES_END);
        mine.end(end);
    });
    Early { rx, ends }
}

/// Every command of `reader` to `forward`, the mic mute applied first (a toggle goes on as the
/// state it set, so the later handler sets the same value again and reports it).
pub(crate) fn read_early<R: BufRead>(reader: R, mut forward: impl FnMut(Command)) -> stdin_reader::ReaderEnd {
    stdin_reader::read_commands(reader, |cmd| forward(apply_mute(cmd)))
}

#[cfg(feature = "encode")]
fn apply_mute(cmd: Command) -> Command {
    match cmd {
        Command::ToggleMicMute => Command::SetMicMute { muted: audio::toggle_mic_mute() },
        Command::SetMicMute { muted } => {
            audio::set_mic_muted(muted);
            cmd
        }
        cmd => cmd,
    }
}

#[cfg(not(feature = "encode"))]
fn apply_mute(cmd: Command) -> Command {
    cmd
}

/// Apply the commands read by [`spawn_early`], from now on. `save` triggers a replay-buffer save
/// (None when encode is off).
pub(crate) fn serve(early: Early, shared: Arc<CaptureShared>, (save, export): (Option<SaveHook>, Option<ExportHook>), extras: engine_extras::Extras) {
    std::thread::spawn(move || serve_with(early, |cmd| apply(cmd, &shared, (&save, &export), &extras)));
}

/// The serve loop: every command in order, then the end of the stream (R2-rel-1: `quit` only
/// after the commands read before it were applied).
pub(crate) fn serve_with(early: Early, mut apply: impl FnMut(Command)) {
    for item in early.rx {
        match item {
            Item::Cmd(cmd) => apply(cmd),
            Item::End(end) => {
                early.ends.end(end);
                let _ = early.ends.taken.send(());
            }
        }
    }
}

fn apply(cmd: Command, shared: &CaptureShared, (save, export): (&Option<SaveHook>, &Option<ExportHook>), extras: &engine_extras::Extras) {
    match cmd {
        Command::SetMode { value } => shared.set_select(&value),
        Command::SaveClip { seconds, at_last_frame } => match save {
            Some(hook) => hook(seconds, at_last_frame),
            None => protocol::emit(&protocol::Outbound::SaveFailed { reason: "engine built without the encode feature".to_string(), rejected: true }),
        },
        Command::ExportClip { path, output, fps, edit } => match export {
            Some(hook) => hook(path, output, fps, edit),
            None => protocol::emit(&protocol::Outbound::ExportFailed { source: path, reason: "engine built without export support".into() }),
        },
        // The reader applied the mute in order. Report the current state without replaying an older mute.
        Command::ToggleMicMute | Command::SetMicMute { .. } => {
            #[cfg(feature = "encode")]
            protocol::emit(&protocol::Outbound::MicMuted { muted: audio::mic_muted() });
        }
        Command::SetGameFolderChoices { choices } => crate::naming::set_choices(choices),
        Command::SetSource { id, gain_db, name, mic_mute, device_id } => {
            #[cfg(feature = "encode")]
            {
                let change = audio::sources::LiveChange { gain_db, name: name.as_deref(), mic_mute, device_id: device_id.as_deref() };
                if !audio::sources::set_live(&id, change) {
                    eprintln!("[recall] set_source: no live track '{id}'");
                }
            }
            #[cfg(not(feature = "encode"))]
            let _ = (id, gain_db, name, mic_mute, device_id);
        }
        cmd @ (Command::Bookmark
        | Command::Screenshot
        | Command::HudProbe
        | Command::RecordStart
        | Command::RecordStop
        | Command::RecordToggle
        | Command::RecordFail { .. }
        | Command::SessionLocked { .. }) => {
            extras.handle(&cmd);
        }
        Command::Quit | Command::StopCapture => {} // the reader ends on them (`stdin_reader`)
    }
}

#[cfg(all(test, feature = "encode"))]
#[path = "engine_stdin_tests.rs"]
mod tests;
