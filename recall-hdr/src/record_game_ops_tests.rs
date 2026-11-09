use super::*;
use crate::types::ClampMode;
use crate::record_config::apply_patch;

/// The shipped shape: an HDR test card, a Never entry, a game with its own clamp (made-up apps).
fn config() -> Config {
    Config {
        overrides: vec![
        Override { exe: "hdr-testcard".into(), kind: RuleKind::HdrGame, clamp: None },
        Override { exe: "OldTool.exe".into(), kind: RuleKind::None, clamp: None },
        Override { exe: "fixturegame".into(), kind: RuleKind::SdrGame, clamp: Some(ClampMode::Accurate) },
        ],
        ..Config::default()
    }
}

fn applied(cfg: &Config, exe: &str, op: GameOp) -> Config {
    let patch = op_patch(cfg, exe, op).expect("a change");
    apply_patch(cfg, &patch).expect("a valid patch").config
}

fn entry<'a>(cfg: &'a Config, exe: &str) -> Option<&'a Override> {
    cfg.overrides.iter().find(|o| crate::rules::same_exe(&o.exe, exe))
}

#[test]
fn record_on_an_app_already_recorded_changes_nothing_r8_detect_4() {
    let cfg = config();
    assert_eq!(op_patch(&cfg, "hdr-testcard", GameOp::Record), None, "hdr_game stays hdr_game");
    assert_eq!(op_patch(&cfg, "FixtureGame.exe", GameOp::Record), None, "its clamp stays");
    assert_eq!(op_patch(&cfg, "oldtool", GameOp::Never), None, "already Never");
}

#[test]
fn a_change_keeps_the_entrys_clamp_and_the_others_r8_detect_4() {
    let never = applied(&config(), "fixturegame", GameOp::Never);
    let e = entry(&never, "fixturegame").unwrap();
    assert_eq!((e.kind, e.clamp), (RuleKind::None, Some(ClampMode::Accurate)));
    assert_eq!(entry(&never, "hdr-testcard").unwrap().kind, RuleKind::HdrGame, "others kept");
    let back = applied(&never, "fixturegame", GameOp::Record);
    let e = entry(&back, "fixturegame").unwrap();
    assert_eq!((e.kind, e.clamp), (RuleKind::SdrGame, Some(ClampMode::Accurate)), "the clamp survives the round trip");
    assert_eq!(back.overrides.len(), 3, "replaced in place, not added");
}

#[test]
fn two_quick_edits_and_a_tray_add_all_land_r8_detect_7() {
    // Each op is applied by the controller against its current file config, one after the
    // other: no whole-array patch built from an older snapshot can drop the earlier one.
    let mut cfg = config();
    cfg = applied(&cfg, "Alpha.exe", GameOp::Record);
    cfg = applied(&cfg, "hdr-testcard", GameOp::Never);
    cfg = apply_patch(&cfg, &tray_add_patch(&cfg, "Beta.exe").unwrap()).unwrap().config;
    cfg = applied(&cfg, "Discord", GameOp::Unlist);
    assert_eq!(entry(&cfg, "alpha").map(|o| o.kind), Some(RuleKind::SdrGame));
    assert_eq!(entry(&cfg, "hdr-testcard").map(|o| o.kind), Some(RuleKind::None));
    assert_eq!(entry(&cfg, "beta").map(|o| o.kind), Some(RuleKind::SdrGame));
    assert!(cfg.detect.fullscreen_games.is_empty());
    assert_eq!(cfg.overrides.len(), 5);
    let removed = applied(&cfg, "ALPHA", GameOp::Remove);
    assert!(entry(&removed, "alpha").is_none() && removed.overrides.len() == 4);
    let listed = applied(&removed, "Chatty.exe", GameOp::Fullscreen);
    assert_eq!(listed.detect.fullscreen_games, ["Chatty.exe"]);
    assert_eq!(op_patch(&listed, "chatty", GameOp::Fullscreen), None, "no duplicate");
    assert_eq!(op_patch(&listed, "Nothing", GameOp::Remove), None);
}

#[test]
fn the_tray_add_never_turns_never_back_into_record_r8_detect_8() {
    let cfg = config();
    assert_eq!(tray_add_patch(&cfg, "OldTool.exe"), None, "Never record stays");
    assert_eq!(tray_add_patch(&cfg, "hdr-testcard"), None, "already a game: nothing to write");
    let added = apply_patch(&cfg, &tray_add_patch(&cfg, "MysteryApp.exe").unwrap()).unwrap().config;
    assert_eq!(entry(&added, "mysteryapp").map(|o| o.kind), Some(RuleKind::SdrGame));
}

#[test]
fn the_tray_offer_retires_once_settings_registers_the_exe_r8_detect_8() {
    let cfg = config();
    assert!(offer_retired(&cfg, "OldTool.exe"), "set to Never in Settings");
    assert!(offer_retired(&cfg, "HDR-Testcard.exe"), "registered in Settings");
    assert!(offer_retired(&cfg, "Discord.exe"), "listed as fullscreen-only");
    assert!(!offer_retired(&cfg, "MysteryApp.exe"), "still unregistered: the offer stays");
}

#[test]
fn ops_parse_from_the_wire() {
    for (s, op) in [("record", GameOp::Record), ("never", GameOp::Never), ("fullscreen", GameOp::Fullscreen), ("remove", GameOp::Remove), ("unlist", GameOp::Unlist)] {
        assert_eq!(GameOp::parse(s), Some(op));
    }
    assert_eq!(GameOp::parse("game"), None);
}
