//! What a user action does once its engine line was, or was not, queued. A line is not
//! queued when no engine runs, its stdin queue is full (`supervisor_stdin.rs`: the
//! engine is not reading), or the engine was told to quit (a game switch or a restart):
//! then the action must not fail silently, and it says why ([`Sent::reason`]).

/// What the caller does, in order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Effect {
    /// Mark the action in flight (`EngineActivity::save_sent` / `command_sent`).
    MarkPending,
    /// Tell the clients (save: the "saving" event; a command: publish status).
    Announce,
    /// The tick cue (save).
    Cue,
    /// The caller's refusal path (notice / failure event + beep), with the reason to show.
    Refused(&'static str),
    /// Logged only (not a user action).
    LogOnly,
    /// An error reply to the pipe client that asked.
    ReplyError,
    /// Do now what the engine's answer would have started (a pause applies at once).
    Proceed,
}

pub const NOT_RESPONDING: &str = "Recorder isn't responding";
/// A line refused because the engine was told to quit for a game switch (`Supervisor::retarget`).
pub const SWITCHING: &str = "Switching games; try again in a moment";
/// A line refused while the engine quits for a planned restart (a settings change, a resume).
pub const RESTARTING: &str = "Recorder is restarting, try again in a moment";

/// What became of one engine line (`Supervisor::send_engine_line`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Sent {
    /// On the engine's stdin queue: the engine reads it before any `quit`.
    Queued,
    /// Not written: the engine was told to quit for a game switch (a retarget), and the
    /// next engine is not up yet.
    Switching,
    /// Not written: the engine was told to quit for a planned restart (new settings, a
    /// resume from sleep), and the next engine is not up yet.
    Restarting,
    /// Not written: the engine was told to stop with no engine to follow (Off: a closed
    /// game's drain ended with no game, a pause).
    Waiting,
    /// Not written: the engine was told to stop for a pause (no engine until resume).
    Paused,
    /// Not written: no engine, a full queue (the engine is not reading), or a quit that
    /// is neither a game switch nor planned (a hang, failure or watchdog restart, shutdown).
    NotResponding,
}

impl Sent {
    pub fn queued(self) -> bool {
        self == Sent::Queued
    }

    /// Of two lines sent in turn: the first refusal (why), else `next`.
    pub fn then(self, next: Sent) -> Sent {
        if self.queued() { next } else { self }
    }

    /// The text a refused user action shows; `None` when queued.
    pub fn reason(self) -> Option<&'static str> {
        match self {
            Sent::Queued => None,
            Sent::Switching => Some(SWITCHING),
            Sent::Restarting => Some(RESTARTING),
            Sent::Waiting => Some(crate::record_engine::WAITING_REASON),
            Sent::Paused => Some(crate::record_engine::PAUSED_REASON),
            Sent::NotResponding => Some(NOT_RESPONDING),
        }
    }
}

/// The reason in a refusal effect, if any.
pub fn refusal(effects: &[Effect]) -> Option<&'static str> {
    effects.iter().find_map(|e| match e {
        Effect::Refused(reason) => Some(*reason),
        _ => None,
    })
}

/// `ok` when queued, else the refusal with its reason, then `rest`.
fn queued_or(sent: Sent, ok: Vec<Effect>, rest: &[Effect]) -> Vec<Effect> {
    match sent.reason() {
        None => ok,
        Some(reason) => std::iter::once(Effect::Refused(reason)).chain(rest.iter().copied()).collect(),
    }
}

/// A save: queued first; only a queued save is marked in flight, announced ("saving") and
/// cued. Not queued: the save-failed event + failure beep (`refuse_capture`) with the
/// reason, no tick and no pending save (a wedged or quitting engine never answers one).
pub fn save_effects(sent: Sent) -> Vec<Effect> {
    queued_or(sent, vec![Effect::MarkPending, Effect::Announce, Effect::Cue], &[])
}

/// Bookmark, screenshot, record start / stop / toggle, export: queued, then marked in
/// flight and announced. Not queued: the same path a refusal takes (bookmark hotkey: its
/// pop-up + beep; screenshot / record: the failure event + beep; a pipe bookmark or
/// record_stop: the error reply; export: `export_failed`).
pub fn command_effects(sent: Sent) -> Vec<Effect> {
    queued_or(sent, vec![Effect::MarkPending, Effect::Announce], &[])
}

/// A capture shortcut's command (bookmark, screenshot, record toggle): as
/// [`command_effects`], then the tick, so a refused one plays none (as a save).
pub fn hotkey_effects(sent: Sent) -> Vec<Effect> {
    queued_or(sent, vec![Effect::MarkPending, Effect::Announce, Effect::Cue], &[])
}

/// Mic mute (to a running engine): not queued shows the refused pop-up + beep (the mute
/// stays the controller's intent and reaches the next engine).
/// The engine's `set_mic_mute` line. `press`: the user's Alt+M or toggle (`set_mic`) only; a
/// resync (a new engine's re-send) is no press, so the engine never counts it as one (an input
/// held on an unsure sibling is released only by a press: gate review P2-b).
pub fn mic_line(muted: bool, press: bool) -> String {
    if press {
        serde_json::json!({"cmd": "set_mic_mute", "muted": muted, "press": true}).to_string()
    } else {
        serde_json::json!({"cmd": "set_mic_mute", "muted": muted}).to_string()
    }
}

pub fn mic_effects(sent: Sent) -> Vec<Effect> {
    queued_or(sent, Vec::new(), &[])
}

/// The island's HUD probe is no user action: a line not queued is only logged.
pub fn hud_probe_effects(sent: Sent) -> Vec<Effect> {
    if sent.queued() { Vec::new() } else { vec![Effect::LogOnly] }
}

/// The legacy (non-record) app's save: not queued sends `save_failed` (pipe) / beeps (hotkey).
pub fn legacy_save_effects(sent: Sent) -> Vec<Effect> {
    queued_or(sent, Vec::new(), &[])
}

/// The legacy app's save (its hotkey, its control-pipe `save_clip`), sent through `send`:
/// the reason to show when the engine did not get it.
pub fn legacy_save(send: impl FnOnce(&str) -> Sent, seconds: Option<u64>) -> Option<&'static str> {
    let line = match seconds {
        Some(s) => serde_json::json!({"cmd": "save_clip", "seconds": s}).to_string(),
        None => r#"{"cmd":"save_clip"}"#.to_string(),
    };
    refusal(&legacy_save_effects(send(&line)))
}

/// A pause's `record_stop` (tray, pipe, hotkey: a user action). Queued: the pause waits
/// for the stop (`PendingPause`, at most `PAUSE_STOP_WAIT`). Not queued: the refused
/// pop-up and beep, and the pause applies at once: waiting for a stop the engine never
/// got gains nothing, and the engine stop that follows ends the recording (its `.part` is
/// recovered on the next start).
pub fn pause_stop_effects(sent: Sent) -> Vec<Effect> {
    queued_or(sent, vec![Effect::MarkPending], &[Effect::Proceed])
}

/// A setting sent live to a running engine from the pipe (`set_mode`, `set_source` gain or
/// name through `set_config`): a user action. Not queued: the refused pop-up + beep, and an
/// error reply to the client. The setting itself is kept (the next engine gets it).
pub fn pipe_setting_effects(sent: Sent) -> Vec<Effect> {
    queued_or(sent, Vec::new(), &[Effect::ReplyError])
}

/// A line no user asked for (a retarget's or sleep's `record_stop`, the intents re-applied
/// to a fresh engine, `record_fail` before a recovery restart): logged only. Each already
/// has its own bound (the auto-stop deadline, the restart on resume, the 20 s wait).
pub fn automatic_effects(sent: Sent) -> Vec<Effect> {
    if sent.queued() { Vec::new() } else { vec![Effect::LogOnly] }
}

/// The controller's save, step by step: `Controller::save_clip` drives it through its
/// own port (`record_cmds.rs`), tests through a recorder of steps.
pub trait SavePort {
    /// Queue the engine line.
    fn queue(&mut self, line: &str) -> Sent;
    fn mark_pending(&mut self);
    fn announce(&mut self, seconds: u32);
    fn cue(&mut self);
    /// The save-failed event + beep, with why.
    fn refused(&mut self, reason: &'static str);
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
    let sent = port.queue(&save_line(seconds, at_last_frame));
    for effect in save_effects(sent) {
        match effect {
            Effect::MarkPending => port.mark_pending(),
            Effect::Announce => port.announce(seconds),
            Effect::Cue => port.cue(),
            Effect::Refused(reason) => port.refused(reason),
            Effect::LogOnly | Effect::ReplyError | Effect::Proceed => {}
        }
    }
    sent.queued()
}

/// The controller's engine command (bookmark, screenshot, record start / stop / toggle),
/// step by step: `Controller::engine_command` drives it through its own port
/// (`record_actions.rs`), tests through a recorder of steps.
pub trait CommandPort {
    /// Queue the engine line.
    fn queue(&mut self, line: &str) -> Sent;
    fn mark_pending(&mut self);
    /// Publish the status (the command is in flight).
    fn announce(&mut self);
    /// The tick cue (a shortcut's).
    fn cue(&mut self);
    /// The command's refusal path, with why.
    fn refused(&mut self, reason: &'static str);
}

/// The command `cmd`, in order: queue first; only a queued one is marked in flight,
/// announced and, for a shortcut (`hotkey`), ticked. What became of the line.
pub fn command(port: &mut impl CommandPort, cmd: &str, hotkey: bool) -> Sent {
    command_line(port, &serde_json::json!({ "cmd": cmd }).to_string(), hotkey)
}

/// Resume recording: `record_start` continuing the recovered recording `path` (its next part).
pub fn resume_line(path: &str) -> String {
    serde_json::json!({"cmd": "record_start", "continues": path}).to_string()
}

/// [`command`] with the engine line given (a command with arguments: Resume recording's
/// `record_start{continues}`).
pub fn command_line(port: &mut impl CommandPort, line: &str, hotkey: bool) -> Sent {
    let sent = port.queue(line);
    let effects = if hotkey { hotkey_effects(sent) } else { command_effects(sent) };
    for effect in effects {
        match effect {
            Effect::MarkPending => port.mark_pending(),
            Effect::Announce => port.announce(),
            Effect::Cue => port.cue(),
            Effect::Refused(reason) => port.refused(reason),
            Effect::LogOnly | Effect::ReplyError | Effect::Proceed => {}
        }
    }
    sent
}

#[cfg(test)]
#[path = "record_dispatch_tests.rs"]
mod tests;
