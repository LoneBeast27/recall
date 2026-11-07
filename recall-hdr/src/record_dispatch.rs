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

#[cfg(test)]
#[path = "record_dispatch_tests.rs"]
mod tests;
