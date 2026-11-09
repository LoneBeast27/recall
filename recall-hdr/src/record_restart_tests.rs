use super::*;
use crate::record_dispatch::Sent;
use crate::record_target::Target;

fn game() -> EngineTarget {
    EngineTarget::Window(Target { pid: 10, hwnd: 0x10, exe: "game".into(), name: "Game".into() })
}

#[test]
fn no_engine_needs_no_restart() {
    assert_eq!(plan_settings_restart(&EngineTarget::Off, false, false), SettingsRestart::Skip);
    assert_eq!(plan_settings_restart(&EngineTarget::Off, true, true), SettingsRestart::Skip);
}

#[test]
fn a_save_or_a_drain_defers_a_settings_restart() {
    assert_eq!(plan_settings_restart(&game(), true, false), SettingsRestart::Defer);
    assert_eq!(plan_settings_restart(&game(), false, true), SettingsRestart::Defer);
}

#[test]
fn only_a_mid_game_settings_restart_tells_the_island() {
    assert_eq!(plan_settings_restart(&game(), false, false), SettingsRestart::Now { notice: true });
    assert_eq!(plan_settings_restart(&EngineTarget::Desktop, false, false), SettingsRestart::Now { notice: false });
}

#[test]
fn settings_and_resume_are_planned_recovery_is_not() {
    assert!(RestartReason::Settings.planned() && RestartReason::Resume.planned());
    assert!(!RestartReason::Recovery.planned());
}

#[test]
fn a_pending_settings_restart_keeps_its_reason() {
    use RestartReason::*;
    assert_eq!(RestartReason::merged(Some(Settings), Resume), Settings);
    assert_eq!(RestartReason::merged(Some(Resume), Settings), Settings);
    assert_eq!(RestartReason::merged(None, Resume), Resume);
}

/// Source guards on the controller's own restart lines (the `Controller` needs a live
/// supervisor, hub and display, so no test builds one): the Sim's `restart_apply` mirrors
/// this one. Every recovery restart goes through `restart_for_recovery`
/// (`transitions_planned_tests.rs`).
#[test]
fn the_controllers_restart_apply_passes_the_reasons_refusal_to_the_supervisor_call() {
    use crate::test_src::method_body;
    let host = method_body(include_str!("record_restart_host.rs"), "restart_apply");
    assert!(host.contains("self.apply_engine_target_as(target, reason.refusal());"), "{host}");
    let ctl = include_str!("record_engine_ctl.rs");
    let retarget = method_body(ctl, "retarget");
    assert!(retarget.contains("OldEngine::Quit(refusal) => switch_engine(&self.supervisor, spec, refusal),"), "{retarget}");
    assert!(method_body(ctl, "apply_engine_target_as").contains("OldEngine::Quit(refusal)"), "a restart quits, never releases");
    assert_eq!(RestartReason::Settings.refusal(), Sent::Restarting);
    assert_eq!(RestartReason::Resume.refusal(), Sent::Restarting);
    assert_eq!(RestartReason::Recovery.refusal(), Sent::NotResponding);
}
