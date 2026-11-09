//! The Sim as the controller's restart host (`record_restart`) and power host
//! (`record_power_host`): the same planner, activity and flag fields, with the supervisor
//! calls logged (`SwitchLog`) and the hub's `save_failed` lines kept (`Sim::hub_events`).

use super::Sim;
use crate::awake_clock::Awake;
use crate::engine_policy::EngineSpec;
use crate::record_activity::EngineActivity;
use crate::record_engine::{EnginePlanner, EngineTarget};
use crate::record_engine_ctl::switch_engine;
use crate::record_power::{PowerAction, PowerPlanner};
use crate::record_power_host::PowerHost;
use crate::record_restart::{restart_engine, RestartHost, RestartReason, Restarted};

impl RestartHost for Sim {
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
        // As `Controller::restart_apply` -> `apply_engine_target_as` -> `switch_engine`.
        let spec = (*target != EngineTarget::Off).then(EngineSpec::default);
        switch_engine(&self.switches, spec, reason.refusal());
        self.applied_engine(target.clone());
    }
    fn restart_notice(&mut self, title: &str, detail: &str) {
        self.island_said.push((title.into(), detail.into()));
    }
    fn restart_log(&mut self, line: &str) {
        self.notes.push(line.into());
    }
}

impl PowerHost for Sim {
    fn power_notices(&mut self) -> Vec<u32> {
        std::mem::take(&mut self.power_notices)
    }
    fn power_planner(&mut self) -> &mut PowerPlanner {
        &mut self.power
    }
    fn power_activity(&mut self) -> &mut EngineActivity {
        &mut self.activity
    }
    fn power_awake(&self) -> Awake {
        self.awake()
    }
    /// As `Controller::apply_power_action`.
    fn power_apply(&mut self, action: PowerAction) {
        match action {
            PowerAction::StopRecording => self.sent.push(("record_stop".into(), self.engine().clone())),
            PowerAction::RestartEngine => {
                PowerHost::power_restart(self);
            }
            PowerAction::Reprobe => {}
        }
    }
    fn power_restart(&mut self) -> bool {
        restart_engine(self, RestartReason::Resume) == Restarted::Now
    }
    fn power_hub_event(&mut self, line: &str) {
        self.hub_events.push(line.into());
    }
    fn power_log(&mut self, line: &str) {
        self.notes.push(line.into());
    }
}

impl Sim {
    /// A power notice (`PBT_*`) the system queued for the pump.
    pub fn power_notice(&mut self, pbt: u32) {
        self.power_notices.push(pbt);
    }

    /// The engine process ended (`ENGINE_EXITED`), then the next engine said hello.
    pub fn engine_exited(&mut self) -> Option<crate::record_activity::Trigger> {
        let (ev, now) = (serde_json::json!({"type": crate::supervisor::ENGINE_EXITED}), self.awake());
        let trigger = self.activity.on_engine_event(&ev, 0.0, now);
        self.activity.engine_restarted();
        trigger
    }
}
