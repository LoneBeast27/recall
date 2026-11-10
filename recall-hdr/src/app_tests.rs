use super::*;
use crate::supervisor::EngineHealth;

#[test]
fn health_summary_maps_all_variants() {
    assert_eq!(health_summary(EngineHealth::Running { pid: 1 }), "running");
    assert_eq!(health_summary(EngineHealth::Restarting { attempt: 1 }), "restarting");
    assert_eq!(health_summary(EngineHealth::GivenUp), "off");
    assert_eq!(health_summary(EngineHealth::Stopped), "stopped");
}

/// The WatchEvent -> Foreground conversion: stem is lowercased, the
/// managed-monitor fullscreen flag (not the any-monitor one) is carried.
#[test]
fn watchevent_to_foreground_lowercases_stem() {
    let ev = WatchEvent {
        pid: 4321,
        exe_stem: "HDR-TestCard".to_string(),
        exe_name: "HDR-TestCard.exe".to_string(),
        title: "Test".to_string(),
        fullscreen: true,
        fullscreen_on_target: false,
        steam_game: false,
        launcher_game: false,
        store_game: false,
        hosted: false,
        hwnd: 0x1234,
    };
    let fg = foreground_of(&ev);
    assert_eq!(fg.exe_stem, "hdr-testcard", "stem must be lowercased");
    assert_eq!(fg.pid, 4321);
    assert!(!fg.fullscreen_on_target, "HDR rules see only the managed monitor");
    assert_eq!(fg.title, "Test");
}

#[test]
fn wait_ms_clamps_to_cap_and_floors_at_zero() {
    assert_eq!(wait_ms(None, 250), 250);
    assert_eq!(wait_ms(Some(Instant::now() - Duration::from_secs(1)), 250), 0);
    assert_eq!(wait_ms(Some(Instant::now() + Duration::from_secs(60)), 250), 250);
}

#[test]
fn the_legacy_focused_rule_refuses_windows_and_recall() {
    use crate::types::RuleKind;
    assert!(super::focused_rule_refused("explorer", RuleKind::HdrGame));
    assert!(super::focused_rule_refused("SystemSettings", RuleKind::SdrGame));
    assert!(super::focused_rule_refused("recall-native", RuleKind::HdrGame));
    assert!(!super::focused_rule_refused("explorer", RuleKind::None), "a none rule is fine");
    assert!(!super::focused_rule_refused("discord", RuleKind::HdrGame), "the user's own app: accepted (and recorded)");
}
