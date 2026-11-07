//! Which supervisor call a restart that is no game switch makes (merge-gate P2 on the
//! save-during-switch work): a settings change and a resume are planned
//! (`Supervisor::restart(.., true)`: "Recorder is restarting" meanwhile); every recovery
//! (hang, encoder failed, the long recording ended early, capture stuck) is not
//! (`restart(.., false)`: "Recorder isn't responding"). Driven through the controller's
//! own restart code (`record_restart::restart_engine` / `request_settings_restart` /
//! `pending_restart_tick`, the Sim as `RestartHost`) down to `record_engine_ctl::switch_engine`,
//! with the decision code of each recovery deciding when to restart.

use std::time::Duration;

use crate::record_capture_watch::{StuckWatch, STUCK_AFTER};
use crate::record_engine::EngineTarget;
use crate::record_recover::{EncoderRecovery, EndDecision, EndingEarly, Recovery};
use crate::record_restart::{RestartReason, Restarted};
use crate::supervisor::EngineHealth;
use crate::transitions_sim::{game_ev, Sim, Switch};

const VAL: &str = "VALORANT-Win64-Shipping";
const UP: EngineHealth = EngineHealth::Running { pid: 9 };

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

/// A hung engine (its heartbeat stops) and a lasting encoder failure: `EncoderRecovery`
/// answers `Restart` (`encoder_recover_tick`, record_recover.rs).
#[test]
fn a_hang_or_encoder_failure_restart_is_no_planned_restart() {
    for (what, state) in [("hang", "recording"), ("failed", "error")] {
        let mut s = in_game();
        let mut rec = EncoderRecovery::default();
        let mut asked = false;
        for sec in 0..=120u64 {
            if rec.step(s.now, state, UP, 3, true, false) == Recovery::Restart {
                assert_eq!(s.restart(RestartReason::Recovery), Restarted::Now, "{what}");
                asked = true;
                break;
            }
            s.run_to(5_000 + sec * 1_000);
        }
        assert!(asked, "{what}: setup: the recovery asked for a restart");
        restarted(&s, false, what);
    }
}

/// The restart after a long recording was told to end early (`EndingEarly`, then
/// `encoder_recover_tick`'s `RestartNow`).
#[test]
fn the_restart_after_a_long_recording_ended_early_is_no_planned_restart() {
    let mut s = in_game();
    let wait = EndingEarly::new(s.now, 1);
    assert_eq!(wait.decide(s.now, false, 1), EndDecision::RestartNow, "setup: the recording ended");
    assert_eq!(s.restart(RestartReason::Recovery), Restarted::Now);
    restarted(&s, false, "ending early");
}

/// No first frame for `STUCK_AFTER` (`StuckWatch`, `capture_stuck_tick`, record_capture_watch.rs).
#[test]
fn a_capture_stuck_restart_is_no_planned_restart() {
    let mut s = in_game();
    let wanted = EngineTarget::Window(s.tracker.current().cloned().expect("setup: A is the target"));
    let mut watch = StuckWatch::default();
    let t = s.now;
    assert_eq!(watch.observe("starting", &wanted, true, true, true, t), None);
    let act = watch.observe("starting", &wanted, true, true, true, t + STUCK_AFTER + Duration::from_millis(1)).expect("stuck");
    assert!(act.restart, "setup: the watchdog asked for its one restart");
    assert_eq!(s.restart(RestartReason::Recovery), Restarted::Now);
    restarted(&s, false, "capture stuck");
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
