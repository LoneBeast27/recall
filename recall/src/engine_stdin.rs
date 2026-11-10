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
        reader_end(end, &shared.quit);
    });
    #[cfg(feature = "encode")]
    std::thread::spawn(|| watch_hold(std::thread::sleep, audio::sources::startup_released, protocol::emit));
    Early(rx)
}

/// How long the device tracks may wait for `sync` before the user is told (a normal start takes
/// a few ms; R-fresh-4: no notice for that).
pub(crate) const HELD_NOTICE_AFTER: std::time::Duration = std::time::Duration::from_secs(2);

/// Why the device tracks are held (`inputs_held`, merge-gate F7).
pub(crate) const INPUTS_HELD_LATE: &str = "the recorder is waiting for its settings";

/// Tell the controller (it shows a notice) when the startup hold still holds after
/// [`HELD_NOTICE_AFTER`] (`wait`: the clock). True when it told. The release says
/// `inputs_released` (`audio::sources`).
pub(crate) fn watch_hold(wait: impl FnOnce(std::time::Duration), released: impl Fn() -> bool, emit: impl FnOnce(&protocol::Outbound)) -> bool {
    wait(HELD_NOTICE_AFTER);
    if released() {
        return false;
    }
    eprintln!("[recall] stdin: no sync from the controller after {HELD_NOTICE_AFTER:?}; the device audio tracks stay held");
    emit(&protocol::Outbound::InputsHeld { reason: INPUTS_HELD_LATE.to_owned() });
    true
}

/// Every command of `reader` to `forward`, the mic mute applied first (a toggle goes on as the
/// state it set, so the later handler sets the same value again and reports it).
pub(crate) fn read_early<R: BufRead>(reader: R, mut forward: impl FnMut(Command)) -> stdin_reader::ReaderEnd {
    stdin_reader::read_commands(reader, |cmd| forward(apply_mute(cmd)))
}

/// The engine's stdin has ended (set once, by the reader): reported in `status.stdin_ended`, so
/// the controller never treats the lines it cannot ack as a hung engine (N2).
static STDIN_ENDED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

pub(crate) fn stdin_ended() -> bool {
    STDIN_ENDED.load(std::sync::atomic::Ordering::SeqCst)
}

/// The reader ended, whichever way: the controller's `quit`, the end of stdin (the controller is
/// gone) or a read error (N2). No line reaches this engine any more, so a privacy change from
/// now on could never apply: every device track stores silence first ([`hold_at_end`]), then
/// `quit` is set (`stdin_reader::finish`), so the long recording's finalise, an orphan's long
/// save and anything else stored until the exit hold no live sample (the safe state).
fn reader_end(end: stdin_reader::ReaderEnd, quit: &std::sync::atomic::AtomicBool) {
    hold_at_end();
    stdin_reader::finish(end, quit);
}

/// The hold of [`reader_end`], and `stdin_ended` for the status.
pub(crate) fn hold_at_end() {
    #[cfg(feature = "encode")]
    audio::sources::hold_for_quit();
    STDIN_ENDED.store(true, std::sync::atomic::Ordering::SeqCst);
}

#[cfg(feature = "encode")]
fn apply_mute(cmd: Command) -> Command {
    apply_audio(cmd)
}

#[cfg(feature = "encode")]
fn apply_audio(cmd: Command) -> Command {
    match cmd {
        Command::ToggleMicMute => Command::SetMicMute { muted: audio::toggle_mic_mute(), press: true },
        Command::SetMicMute { muted, press, .. } => {
            audio::set_mic_muted_by(muted, press);
            cmd
        }
        // A line-in's flag, a re-picked id or a retire read before the tracks start is in force
        // at their first packet (review R2-audio-r9-2), and one read later goes to its live
        // track now. Applied here once, in order: `serve` gets only gain and name, so it can
        // never put an older flag or id back after a newer one (merge-gate F3).
        Command::SetSource { id, gain_db, name, mic_mute, device_id, retire } => {
            if retire == Some(true) {
                audio::sources::stage_retire(&id);
            }
            audio::sources::stage_early(&id, mic_mute, device_id.as_deref());
            Command::SetSource { id, gain_db, name, mic_mute: None, device_id: None, retire: None } // the safe state
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

/// Whether the tracks start held. Only when the controller says it will send `sync`
/// (`RECALL_STDIN_SYNC` set): then only that `sync` releases them. A controller that does not set
/// it (an older build, v0.1.0) never sends `sync`, so holding would silence every track, the game
/// included, for the whole run with no notice; the engine then behaves as v0.1.0 did (the env
/// mute at spawn, re-applied after hello). Merge-gate review of 38e9d9e, P2.
pub(crate) fn startup_hold() -> bool {
    hold_for_sync_env(std::env::var_os(SYNC_ENV).is_some())
}

/// [`startup_hold`] for a given `RECALL_STDIN_SYNC` presence (testable without touching the env).
pub(crate) fn hold_for_sync_env(sync_promised: bool) -> bool {
    if !sync_promised {
        eprintln!("[recall] stdin: {SYNC_ENV} is not set (an older controller); tracks start unheld, as in v0.1.0 (env mute, re-applied after hello)");
    }
    sync_promised
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
        Command::SetSource { id, gain_db, name, mic_mute, device_id, .. } => {
            #[cfg(feature = "encode")]
            {
                let change = audio::sources::LiveChange { gain_db, name: name.as_deref(), mic_mute, device_id: device_id.as_deref(), ..Default::default() };
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
        Command::Quit | Command::StopCapture => {} // the reader ends on them (`stdin_reader`)
    }
}

#[cfg(all(test, feature = "encode"))]
#[path = "engine_stdin_tests.rs"]
mod tests;

#[cfg(all(test, feature = "encode"))]
#[path = "engine_sync_tests.rs"]
mod sync_tests;
