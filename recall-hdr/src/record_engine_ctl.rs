//! The controller's engine control: start, restart or stop the engine for a
//! target, the spec it starts with, and the intents a fresh engine is given
//! (split out of `record_app`).

use crate::engine_policy::EngineSpec;
use crate::record_dispatch::Sent;
use crate::log;
use crate::record_app::Controller;
use crate::record_engine::{engine_env, EngineTarget};
use crate::record_restart::{RestartReason, Restarted};
use crate::supervisor::{EngineHealth, Supervisor};

/// The two supervisor calls an engine (re)start or stop makes (the transitions Sim records
/// them).
pub trait EngineSwitch {
    /// `Supervisor::retarget`: a game switch.
    fn switch_to(&self, spec: Option<EngineSpec>);
    /// `Supervisor::restart`: no game switch; `planned` = a settings change or a resume.
    fn restart_to(&self, spec: Option<EngineSpec>, planned: bool);
    /// `Supervisor::pause`: a stop for a pause.
    fn pause_to(&self);
}

impl EngineSwitch for Supervisor {
    fn switch_to(&self, spec: Option<EngineSpec>) {
        self.retarget(spec);
    }
    fn restart_to(&self, spec: Option<EngineSpec>, planned: bool) {
        self.restart(spec, planned);
    }
    fn pause_to(&self) {
        self.pause();
    }
}

/// `refusal` (what a line sent while the old engine quits is told) picks the call:
/// Switching = a game switch, Restarting = a planned restart, Paused (a stop) = a pause,
/// else a recovery restart. Paused with an engine wanted (no caller does this) is no
/// pause: a planned restart, as a resume's, never "isn't responding".
pub fn switch_engine(sw: &impl EngineSwitch, spec: Option<EngineSpec>, refusal: Sent) {
    match refusal {
        Sent::Switching => sw.switch_to(spec),
        Sent::Paused if spec.is_none() => sw.pause_to(),
        Sent::Paused | Sent::Restarting => sw.restart_to(spec, true),
        _ => sw.restart_to(spec, false),
    }
}

/// How the running engine ends before the next target.
#[derive(Debug, Clone, Copy)]
enum OldEngine {
    /// `quit` (its saves get 3 s); the refusal for lines sent meanwhile.
    Quit(Sent),
    /// `stop_capture`: capture stops now, its saves finish (`supervisor_release.rs`).
    Release,
}

impl Controller {
    /// Start, restart or stop the engine for `target` (an intended restart, a game switch:
    /// a line sent while the old engine quits is told "Switching games").
    pub(crate) fn apply_engine_target(&mut self, target: &EngineTarget) {
        self.apply_engine_target_as(target, Sent::Switching);
    }

    /// `refusal`: what a line sent while the old engine quits is told (Switching for a game
    /// switch, Restarting for a planned restart, NotResponding for a recovery restart).
    pub(crate) fn apply_engine_target_as(&mut self, target: &EngineTarget, refusal: Sent) {
        self.apply_engine_target_how(target, OldEngine::Quit(refusal));
    }

    /// The privacy stop (final review F-detect-1): the old engine stops capture now and
    /// finishes its saves, then `target` starts (`supervisor_release.rs`). `false`: the running
    /// engine does not take `stop_capture` (an older build); nothing was done.
    pub(crate) fn release_engine_to(&mut self, target: &EngineTarget) -> bool {
        if !self.supervisor.engine_takes_stop_capture() {
            return false;
        }
        self.apply_engine_target_how(target, OldEngine::Release);
        true
    }

    fn apply_engine_target_how(&mut self, target: &EngineTarget, how: OldEngine) {
        self.hello_at_retarget = self.supervisor.hello_count();
        // Any (re)start or stop picks up the current settings.
        self.pending_restart = None;
        match target {
            EngineTarget::Off => {
                log!("record: no target -> engine stopped (waiting for a game)");
                recall_trace::act("record.target", &[("kind", "off".into())]);
                self.retarget(None, how);
            }
            EngineTarget::Window(t) => {
                let detected = self.take_detected(t.hwnd);
                log!("record: target -> {:?} ({}, pid {}, hwnd {:#x}){detected}", t.name, t.exe, t.pid, t.hwnd);
                let game = crate::known_games::trace_tag(&t.name, &t.exe);
                recall_trace::act("record.target", &[("kind", "game".into()), ("game", game.into())]);
                self.retarget(Some(self.engine_spec(target)), how);
            }
            EngineTarget::Desktop => {
                log!("record: no game -> desktop capture");
                recall_trace::act("record.target", &[("kind", "desktop".into())]);
                self.retarget(Some(self.engine_spec(target)), how);
            }
        }
    }

    fn retarget(&self, spec: Option<EngineSpec>, how: OldEngine) {
        match how {
            OldEngine::Quit(refusal) => switch_engine(&self.supervisor, spec, refusal),
            OldEngine::Release => self.supervisor.release(spec),
        }
    }

    fn engine_spec(&self, target: &EngineTarget) -> EngineSpec {
        // A new PC's default clip folder (Videos\Recall) may not exist yet.
        if let Err(e) = std::fs::create_dir_all(&self.cfg.recording.output_dir) {
            log!("record: cannot create clip folder {:?}: {e}", self.cfg.recording.output_dir);
        }
        EngineSpec { env: engine_env(target, &self.cfg.recording, self.applied_mask.as_deref(), self.mic_muted) }
    }

    /// Restart a running engine with the wanted target (no game switch). A settings or
    /// resume restart ("Recorder is restarting" meanwhile) waits for a closed game's
    /// drain; a recovery restart ("Recorder isn't responding") never does:
    /// `record_restart::restart_engine`.
    pub(crate) fn restart_engine(&mut self, reason: RestartReason) -> Restarted {
        crate::record_restart::restart_engine(self, reason)
    }

    /// A fresh engine instance starts in `auto` mode with the mute it was spawned with: re-apply
    /// ours. The mute goes either way: a crash respawn reuses the spawn's environment, which may
    /// hold a mute the user has lifted since.
    pub(crate) fn reapply_engine_intents(&mut self) {
        self.reapply_lock();
        // Not a user action: a line the new engine does not take is logged.
        let mute = crate::record_dispatch::mic_line(self.mic_muted, false);
        if !crate::record_dispatch::automatic_effects(self.supervisor.send_engine_line(&mute)).is_empty() && self.mic_muted {
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
