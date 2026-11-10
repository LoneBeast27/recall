//! What the recovery decisions do to the engine (split out of `record_recover` and
//! `record_capture_watch`). Written against [`RestartHost`], so the Sim
//! (`transitions_recovery_tests`) runs the very restart calls the pump ticks make.
//! Every one is a recovery restart: the engine is broken, so it never waits for a drain
//! and is no planned restart ("Recorder isn't responding" meanwhile).

use crate::record_capture_watch::StuckAct;
use crate::record_recover::{restart_lines, EndDecision};
use crate::record_restart::{restart_for_recovery, RestartHost};

/// `encoder_recover_tick` on `Recovery::Restart` (a hang, a lasting encoder failure, a
/// recovery past its bound). With no long recording the engine restarts now and this is
/// empty; else these lines end the recording early first, and the restart follows from
/// the ending-early wait ([`ending_early_step`]).
pub fn recovery_restart(h: &mut impl RestartHost, long_rec: bool) -> Vec<String> {
    let lines = restart_lines(long_rec);
    if lines.is_empty() {
        restart_for_recovery(h);
    }
    lines
}

/// What the ending-early wait did this tick.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EarlyStep {
    Wait,
    /// A new engine came up meanwhile: no restart.
    Cancelled,
    Restarted,
}

/// The ending-early wait's decision applied: its `RestartNow` restarts the engine.
pub fn ending_early_step(h: &mut impl RestartHost, decision: EndDecision) -> EarlyStep {
    match decision {
        EndDecision::Wait => EarlyStep::Wait,
        EndDecision::Cancel => EarlyStep::Cancelled,
        EndDecision::RestartNow => {
            restart_for_recovery(h);
            EarlyStep::Restarted
        }
    }
}

/// `capture_stuck_tick`: the act's one restart (no first frame for [`STUCK_AFTER`]).
///
/// [`STUCK_AFTER`]: crate::record_capture_watch::STUCK_AFTER
pub fn stuck_restart(h: &mut impl RestartHost, act: &StuckAct) -> bool {
    if act.restart {
        restart_for_recovery(h);
    }
    act.restart
}
