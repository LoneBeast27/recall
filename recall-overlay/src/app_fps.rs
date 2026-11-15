//! The rolling FPS record on the live window (`fps_log`): one reading per stats poll, written
//! out on a hitch or when the controller asks (`{"mod":"session","event":{"type":"fps_log"}}`).

use std::time::Instant;

use serde_json::Value;

use super::App;

/// The controller's "write the FPS record now" event.
pub fn is_request(value: &Value) -> bool {
    value.get("mod").and_then(Value::as_str) == Some("session")
        && value.get("event").and_then(|e| e.get("type")).and_then(Value::as_str) == Some("fps_log")
}

impl App {
    /// Keeps the poll's reading; a hitch writes the record out.
    pub(super) fn fps_reading(&mut self, now: Instant) {
        let (fps, low1, shown) = (self.stats.fps, self.stats.low1, self.island.shown());
        if let Some(why) = self.fps_log.push(fps, low1, shown) {
            self.write_fps_log(&format!("hitch: {why}"), now);
        }
    }

    /// Writes the record (no path in smoke runs and tests: nothing written).
    pub(super) fn write_fps_log(&mut self, why: &str, now: Instant) -> bool {
        let Some(path) = self.fps_log_path.clone() else { return false };
        match self.fps_log.dump(&path, now) {
            Ok(true) => {
                self.trace.note("fps log", format!("written ({why}, {} readings)", self.fps_log.len()));
            }
            Ok(false) => {}
            Err(e) => {
                self.trace.note("fps log", format!("not written: {e}"));
            }
        }
        false
    }
}
