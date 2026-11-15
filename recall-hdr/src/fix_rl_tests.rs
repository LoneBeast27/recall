use super::*;

/// A temp `...\TAGame\Config\TASystemSettings.ini` holding `text` (never the real one).
fn settings(tag: &str, text: Option<&str>) -> (PathBuf, String) {
    let root = std::env::temp_dir().join(format!("recall-rl-fix-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let dir = root.join("TAGame").join("Config");
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("TASystemSettings.ini");
    if let Some(t) = text {
        std::fs::write(&file, t).unwrap();
    }
    (root, file.to_string_lossy().into_owned())
}

const FULLSCREEN: &str = "[SystemSettings]\r\nFullscreen=True\r\nResX=2560\r\nBorderless=True\r\n\r\n[SystemSettingsMobile]\r\nFullscreen=True\r\n";

#[test]
fn only_the_games_settings_file_is_a_target() {
    assert!(is_target(r"C:\Users\u\Documents\My Games\Rocket League\TAGame\Config\TASystemSettings.ini"));
    assert!(!is_target(r"C:\x\Config\TASystemSettings.ini"), "not under TAGame");
    assert!(!is_target(r"C:\x\TAGame\Config\TAInput.ini"));
    assert!(is_game(r"G:\Epic\rocketleague\Binaries\Win64\RocketLeague.exe") && !is_game("RocketLeagueX.exe"));
}

#[test]
fn the_plan_lists_only_the_lines_that_differ_from_borderless() {
    let (root, file) = settings("plan", Some(FULLSCREEN));
    let Check::Offer(offer) = plan(&file).unwrap() else { panic!("an offer") };
    assert_eq!(offer.fix, recall_fixes::FixId::RocketLeagueBorderless);
    assert_eq!(offer.steps.len(), 1, "Borderless=True is already set");
    let recall_fixes::Change::IniKey(k) = &offer.steps[0].change else { panic!("an INI change") };
    assert_eq!((k.key.as_str(), k.from.as_deref(), k.to.as_str()), ("Fullscreen", Some("True"), "False"));
    assert!(offer.steps[0].change.exact().iter().any(|l| l.contains("True → False")), "the (i) names the exact change");
    std::fs::write(&file, FULLSCREEN.replace("Fullscreen=True\r\nResX", "Fullscreen=False\r\nResX")).unwrap();
    assert_eq!(plan(&file).unwrap(), Check::AlreadyFixed);
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn no_file_or_no_section_means_no_offer() {
    let (root, file) = settings("none", None);
    assert!(matches!(plan(&file).unwrap(), Check::NotApplicable(_)));
    std::fs::write(&file, "[Other]\r\nA=1\r\n").unwrap();
    assert!(matches!(plan(&file).unwrap(), Check::NotApplicable(_)));
    assert!(plan(r"C:\x\other.ini").is_err());
    let _ = std::fs::remove_dir_all(root);
}

/// Apply then Undo leaves the file byte for byte as it was. (`apply` refuses while the game
/// runs; tests never run while a game does, per the gate's rules.)
#[test]
fn apply_and_undo_round_trip_the_file_exactly() {
    let (root, file) = settings("round", Some(FULLSCREEN));
    let undos = apply(&file).unwrap();
    assert_eq!(undos.len(), 1);
    let after = std::fs::read_to_string(&file).unwrap();
    assert!(after.starts_with("[SystemSettings]\r\nFullscreen=False\r\n") && after.ends_with("[SystemSettingsMobile]\r\nFullscreen=True\r\n"));
    for u in undos.iter().rev() {
        assert_eq!(undo(&file, u).unwrap(), None);
    }
    assert_eq!(std::fs::read_to_string(&file).unwrap(), FULLSCREEN);
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn undo_keeps_a_display_mode_the_user_changed_since() {
    let (root, file) = settings("changed", Some(FULLSCREEN));
    let undos = apply(&file).unwrap();
    std::fs::write(&file, FULLSCREEN.replace("Fullscreen=True\r\nResX", "Fullscreen=Maybe\r\nResX")).unwrap();
    assert!(undo(&file, &undos[0]).unwrap().is_some(), "a note, nothing written");
    assert!(std::fs::read_to_string(&file).unwrap().contains("Fullscreen=Maybe"));
    let other = Undo::IniKey { file: file.clone(), section: SECTION.into(), key: "ResX".into(), value: "1".into(), prior: None };
    assert!(undo(&file, &other).is_err(), "only the display-mode lines this fix sets");
    let _ = std::fs::remove_dir_all(root);
}
