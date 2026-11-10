//! `{"cmd":"quit","save_first":true}` (an update from the Recall app or Setup): before the
//! quit, a long recording in progress is stopped (`record_stop`, it is saved) and the replay
//! buffer is saved (`save_clip`), so nothing recorded is lost when the recorder halts for the
//! update. Both are counted (`EngineActivity::save_sent` / `stop_sent`), so `quit_ready`
//! waits for their `saved` / `recording_stopped` (bounded by the quit's own wait).
//!
//! The replay buffer is saved only when the buffer is on and recording is allowed at that
//! moment: never while paused, with no engine, or while a privacy hold keeps a window from
//! being recorded. A privacy hold wins over `save_first`: nothing is saved or stopped then
//! (the engine's own quit ends the recording as a plain quit does).
//!
//! The `quitting` reply carries `was_recording` (the replay buffer was capturing) and
//! `long_was_on` (a long recording was running or starting), so Setup can restore both after
//! the update.

use serde_json::{json, Value};

use crate::log;

/// What a `save_first` quit does before quitting.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct SaveFirst {
    /// Send `save_clip` (the replay buffer).
    pub save_buffer: bool,
    /// Send `record_stop` (the long recording is saved).
    pub stop_long: bool,
    /// The replay buffer was capturing when the quit came.
    pub was_recording: bool,
    /// A long recording was running or starting when the quit came.
    pub long_was_on: bool,
}

/// The recorder's state at the quit, as [`plan`] reads it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct QuitState {
    /// `capture_refusal` now (paused, no engine, starting): `None` = the buffer captures.
    pub capture_refused: bool,
    /// `EnginePlanner::privacy_refusal` now: a privacy hold is on.
    pub privacy_held: Option<String>,
    /// A long recording is running or starting.
    pub long_on: bool,
    /// A long recording's stop is already in flight (nothing more to send).
    pub stop_in_flight: bool,
}

/// The pipe's `"save_first":true` (anything else is a plain quit).
pub fn wants_save_first(cmd: &Value) -> bool {
    cmd.get("save_first").and_then(Value::as_bool).unwrap_or(false)
}

/// What a `save_first` quit sends, from the state at the quit.
pub fn plan(state: &QuitState) -> SaveFirst {
    let was_recording = !state.capture_refused;
    let long_was_on = state.long_on;
    if state.privacy_held.is_some() {
        return SaveFirst { was_recording, long_was_on, ..SaveFirst::default() };
    }
    SaveFirst {
        save_buffer: was_recording,
        stop_long: state.long_on && !state.stop_in_flight,
        was_recording,
        long_was_on,
    }
}

/// The `quitting` reply's extra fields.
pub fn reply_fields(plan: &SaveFirst) -> Value {
    json!({"was_recording": plan.was_recording, "long_was_on": plan.long_was_on, "save_first": true})
}

impl crate::record_app::Controller {
    /// The recorder's state for [`plan`] now.
    fn quit_state(&self) -> QuitState {
        let refused = crate::record_engine::capture_refusal(self.paused, self.engine_running(), &self.wanted()).is_some();
        QuitState {
            capture_refused: refused,
            privacy_held: self.planner.privacy_refusal(),
            long_on: self.activity.recording_or_starting(),
            stop_in_flight: self.activity.stop_in_flight(),
        }
    }

    /// A `save_first` quit: save what is recorded (once per quit), then the quit's wait
    /// holds until it is on disk. Returns the reply's extra fields.
    pub(crate) fn save_before_quit(&mut self) -> Value {
        let plan = plan(&self.quit_state());
        if !self.quit_wait.take_save_first() {
            return reply_fields(&SaveFirst { save_buffer: false, stop_long: false, ..plan });
        }
        if let Some(reason) = self.planner.privacy_refusal() {
            log!("record: quit save_first -- nothing saved ({reason})");
        }
        if plan.save_buffer {
            log!("record: quit save_first -- saving the replay buffer");
            self.save_clip(None);
        }
        if plan.stop_long {
            log!("record: quit save_first -- stopping the long recording");
            self.send_record_stop();
        }
        reply_fields(&plan)
    }
}

#[cfg(test)]
#[path = "record_quit_save_tests.rs"]
mod tests;
