//! The Sim's stand-in for the supervisor calls (split out of `transitions_sim.rs`, file size).

use crate::engine_policy::EngineSpec;
use crate::record_engine_ctl::EngineSwitch;

/// A supervisor call (`record_engine_ctl::switch_engine`); `stop`: to Off (no spec).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Switch {
    Retarget { stop: bool },
    Restart { stop: bool, planned: bool },
    /// `Supervisor::pause`.
    Pause,
}

/// Records the calls in place of the supervisor.
#[derive(Debug, Default)]
pub struct SwitchLog(pub std::cell::RefCell<Vec<Switch>>);

impl EngineSwitch for SwitchLog {
    fn switch_to(&self, spec: Option<EngineSpec>) {
        self.0.borrow_mut().push(Switch::Retarget { stop: spec.is_none() });
    }
    fn restart_to(&self, spec: Option<EngineSpec>, planned: bool) {
        self.0.borrow_mut().push(Switch::Restart { stop: spec.is_none(), planned });
    }
    fn pause_to(&self) {
        self.0.borrow_mut().push(Switch::Pause);
    }
}

impl SwitchLog {
    pub fn last(&self) -> Option<Switch> {
        self.0.borrow().last().copied()
    }
    pub fn count(&self) -> usize {
        self.0.borrow().len()
    }
}
