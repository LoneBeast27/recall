//! The engine's stdin, split out of `main.rs`. It is read from before any audio track opens
//! (merge gate p9, mute race): an Alt+M the controller sends right after the spawn applies at
//! once (`audio::set_mic_muted`, read by every packet), not after every track has started. The
//! other commands wait in a channel until the encoder and hooks exist ([`serve`]).
//! Every device track starts held and records only after the controller's `sync` line, read in
//! order after its restatement (the sync handshake, `audio::sources::sync_read`): no timing guess.

use std::io::BufRead;
use std::sync::mpsc::{channel, Receiver};
use std::sync::Arc;

use crate::capture::CaptureShared;
use crate::protocol::{self, Command};
use crate::{engine_extras, stdin_reader, ExportHook, SaveHook};
#[cfg(feature = "encode")]
use crate::audio;

/// Commands read so far, for [`serve`].
pub(crate) struct Early(Receiver<Command>);

/// Start reading the process's stdin now; `quit` is set by a `quit` line or the end of stdin.
pub(crate) fn spawn_early(shared: Arc<CaptureShared>) -> Early {
    let (tx, rx) = channel();
    std::thread::spawn(move || {
        let stdin = std::io::stdin();
        let end = read_early(stdin.lock(), |cmd| {
            let _ = tx.send(cmd);
        });
        stdin_reader::finish(end, &shared.quit);
    });
    Early(rx)
}

/// Every command of `reader` to `forward`, the mic mute applied first (a toggle goes on as the
/// state it set, so the later handler sets the same value again and reports it).
pub(crate) fn read_early<R: BufRead>(reader: R, mut forward: impl FnMut(Command)) -> stdin_reader::ReaderEnd {
    stdin_reader::read_commands(reader, |cmd| forward(apply_mute(cmd)))
}

#[cfg(feature = "encode")]
fn apply_mute(cmd: Command) -> Command {
    match cmd {
        Command::ToggleMicMute => Command::SetMicMute { muted: audio::toggle_mic_mute(), press: true },
        Command::SetMicMute { muted, press } => {
            audio::set_mic_muted_by(muted, press);
            cmd
        }
        // A line-in's flag or a re-picked id read before the tracks start is in force at their
        // first packet (review R2-audio-r9-2), and one read later goes to its live track now.
        // Applied here once, in order: `serve` gets only gain and name, so it can never put an
        // older flag or id back after a newer one (merge-gate F3).
        Command::SetSource { id, gain_db, name, mic_mute, device_id } => {
            audio::sources::stage_early(&id, mic_mute, device_id.as_deref());
            Command::SetSource { id, gain_db, name, mic_mute: None, device_id: None } // the safe state
        }
        // The controller's restatement is complete (the sync handshake): the startup hold ends
        // once every track is listed too (`sources::sync_read`).
        Command::Sync { .. } => {
            audio::sources::sync_read();
            cmd
        }
        cmd => cmd,
    }
}

/// Whether the device tracks start held. Always, when a controller drives this engine: only its
/// `sync` releases them. A controller that never sends one (it does not set
/// `RECALL_STDIN_SYNC`: an older build) leaves them held for the run, the safe state.
pub(crate) fn startup_hold() -> bool {
    if std::env::var_os(SYNC_ENV).is_none() {
        eprintln!("[recall] stdin: {SYNC_ENV} is not set (an older controller); the device audio tracks stay held for this run");
    }
    true
}

/// The controller sets this when it will send `sync` after its restatement.
pub(crate) const SYNC_ENV: &str = "RECALL_STDIN_SYNC";

#[cfg(not(feature = "encode"))]
fn apply_mute(cmd: Command) -> Command {
    cmd
}

/// Apply the commands read by [`spawn_early`], from now on. `save` triggers a replay-buffer save
/// (None when encode is off).
pub(crate) fn serve(early: Early, shared: Arc<CaptureShared>, (save, export): (Option<SaveHook>, Option<ExportHook>), extras: engine_extras::Extras) {
    std::thread::spawn(move || {
        for cmd in early.0 {
            apply(cmd, &shared, (&save, &export), &extras);
        }
    });
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
        Command::Sync { .. } => {} // applied by the reader, in order
        Command::Quit => {} // read_commands sets `quit`
    }
}

#[cfg(all(test, feature = "encode"))]
#[path = "engine_stdin_tests.rs"]
mod tests;

#[cfg(all(test, feature = "encode"))]
#[path = "engine_sync_tests.rs"]
mod sync_tests;
