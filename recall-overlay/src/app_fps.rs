//! The rolling FPS record on the live window (`fps_log`): one reading per stats poll, written
//! out (on a background thread) when a hitch is seen.

use std::time::Instant;

use super::App;
use crate::fps_log::FpsLog;

impl App {
    /// Keeps the poll's reading; a hitch writes the record out.
    pub(super) fn fps_reading(&mut self, now: Instant) {
        let (game, fps, low1, shown) = (self.stats.fps_pid, self.stats.fps, self.stats.low1, self.island.shown());
        let Some(why) = self.fps_log.push(game, fps, low1, shown) else { return };
        // No path in smoke runs and tests: nothing is written.
        let Some(path) = self.fps_log_path.clone() else { return };
        if let Some(csv) = self.fps_log.take_dump(now) {
            let rows = self.fps_log.len();
            FpsLog::write_in_background(&path, csv);
            self.trace.note("fps log", format!("written (hitch: {why}, {rows} readings)"));
        }
    }
}
