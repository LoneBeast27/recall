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

/// The text of `fn <name>(` in `src`, up to its closing brace at 4-space indent (a method).
fn method_body<'a>(src: &'a str, name: &str) -> &'a str {
    let start = src.find(&format!("fn {name}(")).unwrap_or_else(|| panic!("fn {name} not found"));
    let end = src[start..].find("\n    }").map(|e| start + e).unwrap_or_else(|| panic!("fn {name} has no end"));
    &src[start..end]
}

/// Source guards on the controller's own restart lines (the `Controller` needs a live
/// supervisor, hub and display, so no test builds one): the Sim's `restart_apply` mirrors
/// this one, and every recovery restart passes `RestartReason::Recovery`.
#[test]
fn the_controllers_restart_apply_passes_the_reasons_refusal_to_the_supervisor_call() {
    let host = method_body(include_str!("record_restart_host.rs"), "restart_apply");
    assert!(host.contains("self.apply_engine_target_as(target, reason.refusal());"), "{host}");
    let ctl = include_str!("record_engine_ctl.rs");
    let apply = method_body(ctl, "apply_engine_target_as");
    assert_eq!(apply.matches(", refusal);").count(), 3, "every target arm passes the refusal on: {apply}");
    assert!(method_body(ctl, "retarget").contains("switch_engine(&self.supervisor, spec, refusal);"));
    assert_eq!(RestartReason::Settings.refusal(), Sent::Restarting);
    assert_eq!(RestartReason::Resume.refusal(), Sent::Restarting);
    assert_eq!(RestartReason::Recovery.refusal(), Sent::NotResponding);
}

#[test]
fn every_recovery_restart_call_site_is_no_planned_restart() {
    // encoder_recover_tick (ending early, hang / encoder failed) and capture_stuck_tick.
    let sites: Vec<&str> = [include_str!("record_recover.rs"), include_str!("record_capture_watch.rs")]
        .iter()
        .flat_map(|src| src.lines().filter(|l| l.contains("self.restart_engine(")))
        .collect();
    assert_eq!(sites.len(), 3, "{sites:?}");
    for site in sites {
        assert!(site.trim_end().ends_with("restart_engine(crate::record_restart::RestartReason::Recovery);"), "{site}");
    }
}
