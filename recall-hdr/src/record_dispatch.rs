//! What a user action does once its engine line was, or was not, queued. A line is not
//! queued when no engine runs or its stdin queue is full (`supervisor_stdin.rs`: the
//! engine is not reading); then the action must not fail silently.

/// What the caller does, in order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Effect {
    /// Mark the action in flight (`EngineActivity::save_sent` / `command_sent`).
    MarkPending,
    /// Tell the clients (save: the "saving" event; a command: publish status).
    Announce,
    /// The tick cue (save).
    Cue,
    /// The caller's "Recorder isn't responding" path (notice / failure event + beep).
    NotResponding,
    /// Logged only (not a user action).
    LogOnly,
    /// An error reply to the pipe client that asked.
    ReplyError,
    /// Do now what the engine's answer would have started (a pause applies at once).
    Proceed,
}

pub const NOT_RESPONDING: &str = "Recorder isn't responding";

/// A save: queued first; only a queued save is marked in flight, announced ("saving") and
/// cued. Not queued: the save-failed event + failure beep (`refuse_capture`), no tick and
/// no pending save (a wedged engine never answers one).
pub fn save_effects(queued: bool) -> Vec<Effect> {
    if queued { vec![Effect::MarkPending, Effect::Announce, Effect::Cue] } else { vec![Effect::NotResponding] }
}

/// Bookmark, screenshot, record start / stop / toggle, export: queued, then marked in
/// flight and announced. Not queued: the same path a refusal takes (bookmark hotkey: its
/// pop-up + beep; screenshot / record: the failure event + beep; a pipe bookmark or
/// record_stop: the error reply; export: `export_failed`).
pub fn command_effects(queued: bool) -> Vec<Effect> {
    if queued { vec![Effect::MarkPending, Effect::Announce] } else { vec![Effect::NotResponding] }
}

/// Mic mute (to a running engine): not queued shows the refused pop-up + beep (the mute
/// stays the controller's intent and reaches the next engine).
pub fn mic_effects(queued: bool) -> Vec<Effect> {
    if queued { Vec::new() } else { vec![Effect::NotResponding] }
}

/// The island's HUD probe is no user action: a line not queued is only logged.
pub fn hud_probe_effects(queued: bool) -> Vec<Effect> {
    if queued { Vec::new() } else { vec![Effect::LogOnly] }
}

/// The legacy (non-record) app's save: not queued sends `save_failed` (pipe) / beeps (hotkey).
pub fn legacy_save_effects(queued: bool) -> Vec<Effect> {
    if queued { Vec::new() } else { vec![Effect::NotResponding] }
}

/// A pause's `record_stop` (tray, pipe, hotkey: a user action). Queued: the pause waits
/// for the stop (`PendingPause`, at most `PAUSE_STOP_WAIT`). Not queued: the refused
/// pop-up and beep, and the pause applies at once: waiting for a stop the engine never
/// got gains nothing, and the engine stop that follows ends the recording (its `.part` is
/// recovered on the next start).
pub fn pause_stop_effects(queued: bool) -> Vec<Effect> {
    if queued { vec![Effect::MarkPending] } else { vec![Effect::NotResponding, Effect::Proceed] }
}

/// A setting sent live to a running engine from the pipe (`set_mode`, `set_source` gain or
/// name through `set_config`): a user action. Not queued: the refused pop-up + beep, and an
/// error reply to the client. The setting itself is kept (the next engine gets it).
pub fn pipe_setting_effects(queued: bool) -> Vec<Effect> {
    if queued { Vec::new() } else { vec![Effect::NotResponding, Effect::ReplyError] }
}

/// A line no user asked for (a retarget's or sleep's `record_stop`, the intents re-applied
/// to a fresh engine, `record_fail` before a recovery restart): logged only. Each already
/// has its own bound (the auto-stop deadline, the restart on resume, the 20 s wait).
pub fn automatic_effects(queued: bool) -> Vec<Effect> {
    if queued { Vec::new() } else { vec![Effect::LogOnly] }
}

/// The controller's save, step by step: `Controller::save_clip` drives it through its
/// own port (`record_cmds.rs`), tests through a recorder of steps.
pub trait SavePort {
    /// Queue the engine line; true when queued.
    fn queue(&mut self, line: &str) -> bool;
    fn mark_pending(&mut self);
    fn announce(&mut self, seconds: u32);
    fn cue(&mut self);
    fn not_responding(&mut self);
}

/// The `save_clip` engine line (`at_last_frame`: a drain, the clip ends at the game's last frame).
pub fn save_line(seconds: u32, at_last_frame: bool) -> String {
    let mut cmd = serde_json::json!({"cmd": "save_clip", "seconds": seconds});
    if at_last_frame {
        cmd["at_last_frame"] = serde_json::Value::Bool(true);
    }
    cmd.to_string()
}

/// The save, in order: queue first; only a queued save is marked pending, announced and
/// ticked (`save_effects`). True when queued.
pub fn save(port: &mut impl SavePort, seconds: u32, at_last_frame: bool) -> bool {
    let queued = port.queue(&save_line(seconds, at_last_frame));
    for effect in save_effects(queued) {
        match effect {
            Effect::MarkPending => port.mark_pending(),
            Effect::Announce => port.announce(seconds),
            Effect::Cue => port.cue(),
            Effect::NotResponding => port.not_responding(),
            Effect::LogOnly | Effect::ReplyError | Effect::Proceed => {}
        }
    }
    queued
}

#[cfg(test)]
#[path = "record_dispatch_tests.rs"]
mod tests;
