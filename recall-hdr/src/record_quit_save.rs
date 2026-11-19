//! `{"cmd":"quit","save_first":true}` (an update, from Setup 0.1.8 or older; newer Setup stops
//! the long recording itself and sends a plain quit): before the quit, a long recording in
//! progress is stopped (`record_stop`, it is saved), so a recording the user started is not
//! cut short by the update. The stop is counted (`EngineActivity::stop_sent`), so
//! `quit_ready` waits for its `recording_stopped` (bounded by the quit's own wait).
//!
//! The replay buffer is NEVER saved by a quit: an update must not turn the buffer into a clip
//! the user did not ask for. A privacy hold wins over `save_first`: nothing is stopped then
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

    /// A `save_first` quit: stop a long recording (once per quit), then the quit's wait
    /// holds until it is on disk. The replay buffer is never saved. Returns the reply's extra
    /// fields.
    pub(crate) fn save_before_quit(&mut self) -> Value {
        let plan = plan(&self.quit_state());
        if !self.quit_wait.take_save_first() {
            return reply_fields(&SaveFirst { stop_long: false, ..plan });
        }
        if let Some(reason) = self.planner.privacy_refusal() {
            log!("record: quit save_first -- nothing stopped ({reason})");
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
