//! The controller's engine control: start, restart or stop the engine for a
//! target, the spec it starts with, and the intents a fresh engine is given
//! (split out of `record_app`).

use crate::engine_policy::EngineSpec;
use crate::log;
use crate::record_app::Controller;
use crate::record_engine::{engine_env, EngineTarget};
use crate::supervisor::EngineHealth;

impl Controller {
    /// Start, restart or stop the engine for `target` (an intended restart, a game switch:
    /// a line sent while the old engine quits is told "Switching games").
    pub(crate) fn apply_engine_target(&mut self, target: &EngineTarget) {
        self.apply_engine_target_as(target, true);
    }

    /// `switching` false: a restart that is no game switch (hang, failure, settings,
    /// resume); a line sent while the old engine quits is told "Recorder isn't responding".
    fn apply_engine_target_as(&mut self, target: &EngineTarget, switching: bool) {
        self.hello_at_retarget = self.supervisor.hello_count();
        // Any (re)start or stop picks up the current settings.
        self.pending_restart = false;
        match target {
            EngineTarget::Off => {
                log!("record: no target -> engine stopped (waiting for a game)");
                self.retarget(None, switching);
            }
            EngineTarget::Window(t) => {
                let detected = self.take_detected(t.hwnd);
                log!("record: target -> {:?} ({}, pid {}, hwnd {:#x}){detected}", t.name, t.exe, t.pid, t.hwnd);
                self.retarget(Some(self.engine_spec(target)), switching);
            }
            EngineTarget::Desktop => {
                log!("record: no game -> desktop capture");
                self.retarget(Some(self.engine_spec(target)), switching);
            }
        }
    }

    fn retarget(&self, spec: Option<EngineSpec>, switching: bool) {
        if switching { self.supervisor.retarget(spec) } else { self.supervisor.restart(spec) }
    }

    fn engine_spec(&self, target: &EngineTarget) -> EngineSpec {
        // A new PC's default clip folder (Videos\Recall) may not exist yet.
        if let Err(e) = std::fs::create_dir_all(&self.cfg.recording.output_dir) {
            log!("record: cannot create clip folder {:?}: {e}", self.cfg.recording.output_dir);
        }
        EngineSpec { env: engine_env(target, &self.cfg.recording, self.applied_mask.as_deref()) }
    }

    /// Restart a running engine so new buffer/output/affinity settings apply.
    pub(crate) fn restart_engine_for_settings(&mut self) {
        if *self.planner.applied() == EngineTarget::Off {
            return; // the next start already uses the new settings
        }
        let wanted = self.wanted();
        self.planner.force(wanted.clone());
        self.apply_engine_target_as(&wanted, false);
    }

    /// A fresh engine instance starts unmuted in `auto` mode: re-apply ours.
    pub(crate) fn reapply_engine_intents(&mut self) {
        // Not a user action: a line the new engine does not take is logged.
        if self.mic_muted && !crate::record_dispatch::automatic_effects(self.supervisor.send_engine_line(r#"{"cmd":"set_mic_mute","muted":true}"#)).is_empty() {
            log!("record: the new engine did not take the mic mute ({})", crate::record_dispatch::NOT_RESPONDING);
        }
        if let Some(mode) = self.mode_select.as_deref().filter(|m| *m != "auto") {
            if !crate::record_dispatch::automatic_effects(self.supervisor.send_engine_line(&serde_json::json!({"cmd": "set_mode", "value": mode}).to_string())).is_empty() {
                log!("record: the new engine did not take the capture mode ({})", crate::record_dispatch::NOT_RESPONDING);
            }
        }
    }

    pub(crate) fn engine_running(&self) -> bool {
        matches!(self.supervisor.health(), EngineHealth::Running { pid } if pid != 0)
    }
}
