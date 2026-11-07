//! Which supervisor call a restart that is no game switch makes (merge-gate P2 on the
//! save-during-switch work): a settings change and a resume are planned
//! (`Supervisor::restart(.., true)`: "Recorder is restarting" meanwhile); every recovery
//! (hang, encoder failed, the long recording ended early, capture stuck) is not
//! (`restart(.., false)`: "Recorder isn't responding"). Driven through the controller's
//! own restart code (`record_restart::restart_engine` / `request_settings_restart` /
//! `pending_restart_tick`, the Sim as `RestartHost`) down to `record_engine_ctl::switch_engine`.
//! Every recovery call site (`encoder_recover_tick`, `capture_stuck_tick`) calls
//! `Controller::restart_for_recovery` -> `record_restart::restart_for_recovery` (`Sim::recover`).

use crate::record_restart::{RestartReason, Restarted};
use crate::engine_policy::EngineSpec;
use crate::record_dispatch::Sent;
use crate::record_engine_ctl::switch_engine;
use crate::transitions_sim::{game_ev, Sim, Switch, SwitchLog};

const VAL: &str = "VALORANT-Win64-Shipping";

/// A recorded mid-game (no drain, nothing in flight).
fn in_game() -> Sim {
    let mut s = Sim::new();
    s.focus(game_ev(VAL, 10, 0x10));
    s.run_to(5_000);
    assert!(s.recording_of(10), "setup: A is recorded");
    assert_eq!(s.switches.count(), 0, "setup: no restart yet");
    s
}

fn restarted(s: &Sim, planned: bool, what: &str) {
    assert_eq!(s.switches.count(), 1, "{what}: one supervisor call");
    assert_eq!(s.switches.last(), Some(Switch::Restart { stop: false, planned }), "{what}");
}

#[test]
fn a_settings_change_and_a_resume_are_planned_restarts() {
    let mut s = in_game();
    s.settings_changed();
    restarted(&s, true, "settings");
    let mut s = in_game();
    assert_eq!(s.restart(RestartReason::Resume), Restarted::Now);
    restarted(&s, true, "resume");
}

/// Deferred for a closed game's drain (the window is gone, then the same window is back):
/// the restart `pending_restart_tick` runs later is still planned.
#[test]
fn a_deferred_settings_or_resume_restart_is_still_planned() {
    for reason in [RestartReason::Settings, RestartReason::Resume] {
        let mut s = in_game();
        s.window_gone(0x10);
        s.run_to(5_100);
        assert!(s.planner.drain().is_some(), "{reason:?}: setup: A's window drains");
        if reason == RestartReason::Settings {
            s.settings_changed();
        } else {
            assert_eq!(s.restart(reason), Restarted::Deferred);
        }
        assert_eq!(s.switches.count(), 0, "{reason:?}: waits for the drain");
        s.focus(game_ev(VAL, 10, 0x10));
        s.run_to(8_000);
        restarted(&s, true, &format!("{reason:?} after the drain"));
    }
}

/// A recovery during a closed game's drain does not wait, and is still no planned restart.
#[test]
fn a_recovery_during_a_drain_restarts_at_once_unplanned() {
    let mut s = in_game();
    s.exit(10);
    s.run_to(5_100);
    assert!(s.planner.drain().is_some(), "setup: A drains");
    assert_eq!(s.restart(RestartReason::Recovery), Restarted::Now);
    assert_eq!(s.switches.last(), Some(Switch::Restart { stop: true, planned: false }), "to Off: no game is wanted");
}

/// `restart_for_recovery`, the helper every recovery call site uses: mid-game and during a
/// drain it restarts at once, never planned ("Recorder isn't responding" meanwhile).
#[test]
fn the_recovery_helper_restarts_at_once_unplanned() {
    let mut s = in_game();
    assert_eq!(s.recover(), Restarted::Now);
    restarted(&s, false, "mid-game");
    let mut s = in_game();
    s.window_gone(0x10);
    s.run_to(5_100);
    assert!(s.planner.drain().is_some(), "setup: A's window drains");
    assert_eq!(s.recover(), Restarted::Now, "a broken engine never waits for the drain");
    assert_eq!(s.switches.last().map(|w| matches!(w, Switch::Restart { planned: false, .. })), Some(true));
}

/// No caller pauses with an engine wanted; if one did, it is no pause and no recovery: a
/// planned restart ("Recorder is restarting"), not "isn't responding".
#[test]
fn a_pause_refusal_with_an_engine_wanted_is_a_planned_restart() {
    let log = SwitchLog::default();
    switch_engine(&log, Some(EngineSpec::default()), Sent::Paused);
    assert_eq!(log.last(), Some(Switch::Restart { stop: false, planned: true }));
    switch_engine(&log, None, Sent::Paused);
    assert_eq!(log.last(), Some(Switch::Pause));
}
