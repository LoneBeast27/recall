//! The engine's stdin, split out of `main.rs`. It is read from before any audio track opens
//! (merge gate p9, mute race): an Alt+M the controller sends right after the spawn applies at
//! once (`audio::set_mic_muted`, read by every packet), not after every track has started. The
//! other commands wait in a channel until the encoder and hooks exist ([`serve`]).

use std::io::BufRead;
use std::sync::atomic::AtomicBool;
use std::sync::mpsc::{channel, Receiver};
use std::sync::Arc;

use crate::capture::CaptureShared;
use crate::protocol::{self, Command};
use crate::{engine_extras, stdin_commands, ExportHook, SaveHook};
#[cfg(feature = "encode")]
use crate::audio;

/// Commands read so far, for [`serve`].
pub(crate) struct Early(Receiver<Command>);

/// Start reading the process's stdin now; `quit` is set by a `quit` line or the end of stdin.
pub(crate) fn spawn_early(shared: Arc<CaptureShared>) -> Early {
    let (tx, rx) = channel();
    std::thread::spawn(move || {
        let stdin = std::io::stdin();
        read_early(stdin.lock(), &shared.quit, |cmd| {
            let _ = tx.send(cmd);
        });
    });
    Early(rx)
}

/// Every command of `reader` to `forward`, the mic mute applied first (a toggle goes on as the
/// state it set, so the later handler sets the same value again and reports it).
pub(crate) fn read_early<R: BufRead>(reader: R, quit: &AtomicBool, mut forward: impl FnMut(Command)) {
    stdin_commands::read_commands(reader, quit, |cmd| forward(apply_mute(cmd)));
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
            None => protocol::emit(&protocol::Outbound::SaveFailed { reason: "engine built without the encode feature".to_string() }),
        },
        Command::ExportClip { path, output, fps, edit } => match export {
            Some(hook) => hook(path, output, fps, edit),
            None => protocol::emit(&protocol::Outbound::ExportFailed { source: path, reason: "engine built without export support".into() }),
        },
        // `read_early` turned every toggle into the state it set; report what is in force.
        Command::ToggleMicMute => {
            #[cfg(feature = "encode")]
            protocol::emit(&protocol::Outbound::MicMuted { muted: audio::mic_muted() });
        }
        Command::SetMicMute { muted } => {
            #[cfg(feature = "encode")]
            {
                audio::set_mic_muted(muted);
                protocol::emit(&protocol::Outbound::MicMuted { muted });
            }
            #[cfg(not(feature = "encode"))]
            let _ = muted;
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
        Command::Quit => {} // read_commands sets `quit`
    }
}

#[cfg(all(test, feature = "encode"))]
#[path = "engine_stdin_tests.rs"]
mod tests;
