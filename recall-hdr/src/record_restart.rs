//! Engine restarts that are no game switch, on the pump thread (AUDIT-3 U2).
//!
//! Every such restart names its [`RestartReason`]. A recovery restart (a hung engine, a
//! lasting encoder failure, the restart after a long recording ended early, a capture
//! that never showed a frame) happens at once, drain or not: the engine it replaces is
//! broken. A settings or resume restart waits for a closed game's drain
//! (`pending_restart` carries the reason), so the drain's last clip stays saveable. The
//! drain's end applies Off, desktop capture or the next game, and any start reads the
//! current settings, so it needs no extra restart; a drain that ends with the engine kept
//! (the same target wanted again) is restarted by [`pending_restart_tick`].
//!
//! The controller (`record_restart_host.rs`) and the transitions Sim both run this code
//! through [`RestartHost`].

use crate::record_dispatch::Sent;
use crate::record_engine::{EnginePlanner, EngineTarget};

/// The island notice for a settings restart that wiped a game's buffer.
pub const NOTICE_TITLE: &str = "Settings applied";
pub const NOTICE_DETAIL: &str = "The replay buffer restarted";

/// Why the engine restarts (not a game switch).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RestartReason {
    /// Recording settings changed (buffer, folder, codec, fps, audio, the desktop's monitor).
    Settings,
    /// The PC woke from sleep: a fresh engine and replay buffer.
    Resume,
    /// The engine is broken (hang, encoder failed, long recording ended early, capture stuck).
    Recovery,
}

impl RestartReason {
    /// A planned restart (settings, resume) as opposed to a recovery from a broken engine.
    pub fn planned(self) -> bool {
        self != RestartReason::Recovery
    }

    /// What a line sent while the old engine quits is told: "Recorder is restarting" for a
    /// planned restart, "Recorder isn't responding" for a recovery (`Supervisor::restart`'s
    /// `planned`, through `record_engine_ctl::switch_engine`).
    pub fn refusal(self) -> Sent {
        if self.planned() { Sent::Restarting } else { Sent::NotResponding }
    }

    /// Two deferred restarts are one: a settings change says "Settings applied".
    fn merged(old: Option<RestartReason>, new: RestartReason) -> RestartReason {
        if old == Some(RestartReason::Settings) { RestartReason::Settings } else { new }
    }
}

/// What [`restart_engine`] did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Restarted {
    /// No engine runs: the next start reads the current settings.
    NoEngine,
    /// A closed game's drain runs: `pending_restart` holds the reason until it ends.
    Deferred,
    Now,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SettingsRestart {
    /// No engine: the next start reads the new settings (`pending_restart` cleared).
    Skip,
    /// A save, long recording or drain is in flight: `pending_restart`, applied once it ends.
    Defer,
    /// Restart now. `notice`: a game's replay buffer is wiped, tell the island.
    Now { notice: bool },
}

/// `applied`: the planner's applied target; `busy`: `EngineActivity::busy`;
/// `draining`: `EnginePlanner::drain().is_some()`.
pub fn plan_settings_restart(applied: &EngineTarget, busy: bool, draining: bool) -> SettingsRestart {
    if *applied == EngineTarget::Off {
        return SettingsRestart::Skip;
    }
    if busy || draining {
        return SettingsRestart::Defer;
    }
    SettingsRestart::Now { notice: matches!(applied, EngineTarget::Window(_)) }
}

/// What a restart reads and drives.
pub trait RestartHost {
    fn restart_planner(&mut self) -> &mut EnginePlanner;
    /// `EngineActivity::busy` now (a save or long recording in flight).
    fn restart_busy(&mut self) -> bool;
    /// `Controller::wanted`.
    fn restart_wanted(&self) -> EngineTarget;
    /// `Controller::pending_restart`.
    fn restart_pending(&mut self) -> &mut Option<RestartReason>;
    /// Start, restart or stop the engine for `target`, not as a game switch (clears
    /// `pending_restart`).
    fn restart_apply(&mut self, target: &EngineTarget, reason: RestartReason);
    /// `Controller::shortcut_notice`.
    fn restart_notice(&mut self, title: &str, detail: &str);
    /// `log!`.
    fn restart_log(&mut self, line: &str);
    /// New settings reached the controller (encoder recovery forgets a give-up).
    fn restart_settings_changed(&mut self) {}
}

/// Engine settings changed: restart now, or once no save / long recording / drain is in
/// flight (`pending_restart` meanwhile). A mid-game restart wipes the replay buffer, so
/// the island says so.
pub fn request_settings_restart(h: &mut impl RestartHost) {
    h.restart_settings_changed();
    let draining = h.restart_planner().drain().is_some();
    let busy = h.restart_busy();
    let applied = h.restart_planner().applied().clone();
    match plan_settings_restart(&applied, busy, draining) {
        SettingsRestart::Skip => *h.restart_pending() = None,
        SettingsRestart::Defer => {
            if h.restart_pending().is_none() {
                let what = if draining { "the closed game's drain" } else { "the save / long recording" };
                h.restart_log(&format!("record: engine settings changed; restart deferred until {what} ends"));
            }
            *h.restart_pending() = Some(RestartReason::Settings);
        }
        SettingsRestart::Now { notice } => {
            h.restart_log("record: engine settings changed; restarting the engine (the replay buffer restarts)");
            if restart_engine(h, RestartReason::Settings) == Restarted::Now && notice {
                h.restart_notice(NOTICE_TITLE, NOTICE_DETAIL);
            }
        }
    }
}

/// Restart a running engine with the wanted target. A planned restart during a closed
/// game's drain only sets `pending_restart`; a recovery restart never waits.
pub fn restart_engine(h: &mut impl RestartHost, reason: RestartReason) -> Restarted {
    if *h.restart_planner().applied() == EngineTarget::Off {
        *h.restart_pending() = None; // the next start already uses the new settings
        return Restarted::NoEngine;
    }
    if reason.planned() && h.restart_planner().drain().is_some() {
        let old = *h.restart_pending();
        if old.is_none() {
            h.restart_log(&format!("record: engine restart ({reason:?}) deferred until the closed game's drain ends"));
        }
        *h.restart_pending() = Some(RestartReason::merged(old, reason));
        return Restarted::Deferred;
    }
    let wanted = h.restart_wanted();
    h.restart_planner().force(wanted.clone());
    h.restart_apply(&wanted, reason);
    Restarted::Now
}

/// Every pump tick (`activity_tick`): a deferred restart once nothing is in flight. A
/// settings one goes through the settings path ("Settings applied" mid-game); a resume
/// one restarts quietly, as a resume does.
pub fn pending_restart_tick(h: &mut impl RestartHost) {
    let Some(reason) = *h.restart_pending() else { return };
    // Still in flight: the drain's own end (or the save's) comes first.
    if h.restart_busy() || h.restart_planner().drain().is_some() {
        return;
    }
    match reason {
        RestartReason::Settings => request_settings_restart(h),
        _ => {
            if restart_engine(h, reason) == Restarted::Now {
                h.restart_log(&format!("record: the deferred engine restart ({reason:?}) ran"));
            }
        }
    }
}

#[cfg(test)]
#[path = "record_restart_tests.rs"]
mod tests;
