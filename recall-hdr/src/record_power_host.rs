//! Sleep and resume as events through a seam (design-detect-release P14/P15): the rules are
//! [`PowerHost::power_tick`], a provided method, so the record controller
//! (`record_power_win`) and the transitions Sim run the same code; each only supplies the
//! queued notices, its state and the actions.
//!
//! Suspend never holds anything: the awake-only clock stops with the PC, so no save window,
//! cap or settle ages during the sleep. A resume's engine restart waits on facts
//! (`EngineActivity::resume_gate`): while a save is still in flight and still making progress,
//! up to [`crate::record_activity::RESUME_HOLD_MAX`] of awake time. Product rule (orchestrator,
//! 2026-11-10): a save past its 10 min hold cap keeps holding while it moves. Once it stops (or
//! the bound passes) the restart goes and every save it cuts is reported once
//! ([`RESUME_LOST_REASON`]), never cut silently. A resume during a privacy release restarts
//! only the next engine; the released one drains on.

use serde_json::json;

use crate::awake_clock::Awake;
use crate::record_activity::EngineActivity;
use crate::record_power::{PowerAction, PowerEvent, PowerPlanner};

/// Why a save the resume restart cut failed.
pub const RESUME_LOST_REASON: &str = "The recorder restarted after sleep";

/// The `save_failed` each such save gets (mod `engine`, as the engine's own).
pub fn resume_lost_event() -> String {
    json!({"type": "save_failed", "reason": RESUME_LOST_REASON}).to_string()
}

/// What the sleep/resume rules read and drive.
pub(crate) trait PowerHost {
    /// The power notices (`PBT_*`) queued since the last call (`record_power_win::take`).
    fn power_notices(&mut self) -> Vec<u32>;
    fn power_planner(&mut self) -> &mut PowerPlanner;
    fn power_activity(&mut self) -> &mut EngineActivity;
    /// Now on the awake-only clock.
    fn power_awake(&self) -> Awake;
    /// Apply a stop or a re-probe (`Controller::apply_power_action`).
    fn power_apply(&mut self, action: PowerAction);
    /// The resume's engine restart (`restart_engine(Resume)`); true only when it ran now (a
    /// closed game's drain defers it, with no engine there is none).
    fn power_restart(&mut self) -> bool;
    /// One line to the app clients (`PipeHub::event("engine", ..)`).
    fn power_hub_event(&mut self, line: &str);
    fn power_log(&mut self, line: &str);

    /// Pump step, before `tick`: act on the notices that arrived since the last pass.
    fn power_tick(&mut self) {
        let now = self.power_awake();
        let mut asked = Vec::new();
        for event in self.power_notices().into_iter().filter_map(PowerEvent::from_pbt) {
            let recording = self.power_activity().is_recording();
            self.power_log(&format!("record: power notice {event:?} (long recording running: {recording})"));
            asked.extend(self.power_planner().on_event(event, recording));
        }
        let was_held = self.power_planner().held();
        // Facts first, one borrow at a time: how long a restart has waited, then the saves.
        let held_for = self.power_planner().held_for(now);
        let verdict = self.power_activity().resume_gate(now, held_for);
        let (actions, lost) = self.power_planner().gate(asked, now, verdict);
        if self.power_planner().held() && !was_held {
            self.power_log("record: resumed from sleep -- engine restart deferred until the save being written ends");
        }
        for action in actions {
            if action != PowerAction::RestartEngine {
                self.power_apply(action);
                continue;
            }
            // R2-rel-2: the cut saves are reported only when the restart ran; a deferred one
            // (a closed game's drain) cuts nothing yet, and the save may still land. Whatever
            // the later restart cuts is reported at the engine's exit (P13).
            if self.power_restart() && lost > 0 {
                self.power_log(&format!("record: resumed from sleep -- the engine restarted with {lost} save(s) stopped in flight (reported failed)"));
                self.power_activity().forget_lost();
                for _ in 0..lost {
                    self.power_hub_event(&resume_lost_event());
                }
            }
        }
    }
}
