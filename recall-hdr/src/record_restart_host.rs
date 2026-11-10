//! `record_app::Controller` as the host of the shared restart code (`record_restart`):
//! `restart_engine`, `request_engine_restart` and `activity_tick` call through it.

use crate::log;
use crate::record_app::Controller;
use crate::record_engine::{EnginePlanner, EngineTarget};
use crate::record_restart::{RestartHost, RestartReason};

impl RestartHost for Controller {
    fn restart_planner(&mut self) -> &mut EnginePlanner {
        &mut self.planner
    }
    fn restart_busy(&mut self) -> bool {
        let now = self.awake();
        self.activity.busy(now)
    }
    fn restart_wanted(&self) -> EngineTarget {
        self.wanted()
    }
    fn restart_pending(&mut self) -> &mut Option<RestartReason> {
        &mut self.pending_restart
    }
    fn restart_apply(&mut self, target: &EngineTarget, reason: RestartReason) {
        self.apply_engine_target_as(target, reason.refusal());
    }
    fn restart_notice(&mut self, title: &str, detail: &str) {
        self.shortcut_notice(title, detail);
    }
    fn restart_log(&mut self, line: &str) {
        log!("{line}");
    }
    fn restart_settings_changed(&mut self) {
        // New settings: an encoder recovery that gave up on a hung engine tries again.
        self.recovery.settings_changed();
    }
}
