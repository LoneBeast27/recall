use super::*;

/// The settings file at its real place under an injected temp profile (never the user's),
/// holding `text`. Keep the guard alive for the test.
fn settings(tag: &str, text: Option<&str>) -> (PathBuf, String, crate::user_dirs::test_root::Injected) {
    let root = std::env::temp_dir().join(format!("recall-rl-fix-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let guard = crate::user_dirs::inject(&root);
    let file = config_path().expect("Documents under the temp root");
    std::fs::create_dir_all(file.parent().unwrap()).unwrap();
    if let Some(t) = text {
        std::fs::write(&file, t).unwrap();
    }
    (root, file.to_string_lossy().into_owned(), guard)
}

const FULLSCREEN: &str = "[SystemSettings]\r\nFullscreen=True\r\nResX=2560\r\nBorderless=True\r\n\r\n[SystemSettingsMobile]\r\nFullscreen=True\r\n";
const BOTH: &str = "[SystemSettings]\r\nFullscreen = True\r\nResX=2560\r\nBorderless=False\r\n";

#[test]
fn only_the_settings_file_under_documents_is_a_target() {
    let (root, file, _g) = settings("target", None);
    assert!(is_target(&file) && is_target(&file.to_uppercase()));
    assert!(!is_target(r"C:\x\TAGame\Config\TASystemSettings.ini"), "the same name elsewhere");
    assert!(is_game(r"G:\Epic\rocketleague\Binaries\Win64\RocketLeague.exe") && !is_game("RocketLeagueX.exe"));
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn the_plan_lists_only_the_lines_that_differ_from_borderless() {
    let (root, file, _g) = settings("plan", Some(FULLSCREEN));
    let Check::Offer(offer) = plan(&file).unwrap() else { panic!("an offer") };
    assert_eq!(offer.fix, recall_fixes::FixId::RocketLeagueBorderless);
    assert_eq!(offer.steps.len(), 1, "Borderless=True is already set");
    let recall_fixes::Change::IniKey(k) = &offer.steps[0].change else { panic!("an INI change") };
    assert_eq!((k.key.as_str(), k.from.as_deref(), k.to.as_str()), ("Fullscreen", Some("True"), "False"));
    assert!(offer.steps[0].change.exact().iter().any(|l| l.contains("True → False")), "the (i) names the exact change");
    std::fs::write(&file, FULLSCREEN.replace("Fullscreen=True\r\nResX", "Fullscreen=False\r\nResX")).unwrap();
    assert_eq!(plan(&file).unwrap(), Check::AlreadyFixed);
    std::fs::write(&file, format!("\u{feff}{FULLSCREEN}")).unwrap();
    assert!(matches!(plan(&file).unwrap(), Check::Offer(_)), "a BOM still gets the offer");
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn no_file_or_no_section_means_no_offer() {
    let (root, file, _g) = settings("none", None);
    assert!(matches!(plan(&file).unwrap(), Check::NotApplicable(_)));
    std::fs::write(&file, "[Other]\r\nA=1\r\n").unwrap();
    assert!(matches!(plan(&file).unwrap(), Check::NotApplicable(_)));
    assert!(plan(r"C:\x\other.ini").is_err());
    let _ = std::fs::remove_dir_all(root);
}

/// Apply then Undo leaves the file byte for byte as it was, spacing included, and a backup of
/// the original stays beside it. (`apply` refuses while the game runs; tests never run then.)
#[test]
fn apply_and_undo_round_trip_the_file_exactly() {
    let (root, file, _g) = settings("round", Some(BOTH));
    let undos = apply(&file).unwrap();
    assert_eq!(undos.len(), 2);
    let after = std::fs::read_to_string(&file).unwrap();
    assert_eq!(after, "[SystemSettings]\r\nFullscreen = False\r\nResX=2560\r\nBorderless=True\r\n");
    assert_eq!(std::fs::read_to_string(fix_ini::backup_path(Path::new(&file))).unwrap(), BOTH, "the backup holds the original");
    for u in undos.iter().rev() {
        assert_eq!(undo(&file, u).unwrap(), None);
    }
    assert_eq!(std::fs::read_to_string(&file).unwrap(), BOTH);
    for u in undos.iter().rev() {
        assert_eq!(undo(&file, u).unwrap(), None, "a retry: lines already back count as done");
    }
    let _ = std::fs::remove_dir_all(root);
}

/// The user changes one line after the fix: Undo leaves that line as they set it (a note) and
/// still puts the other line back.
#[test]
fn a_line_the_user_changed_since_does_not_stop_the_other_one() {
    let (root, file, _g) = settings("changed", Some(BOTH));
    let undos = apply(&file).unwrap();
    let mine = std::fs::read_to_string(&file).unwrap().replace("Borderless=True", "Borderless=Maybe");
    std::fs::write(&file, &mine).unwrap();
    let notes: Vec<Option<String>> = undos.iter().rev().map(|u| undo(&file, u).unwrap()).collect();
    assert!(notes[0].as_deref().is_some_and(|n| n.contains("Borderless")), "{notes:?}");
    assert_eq!(notes[1], None, "Fullscreen is still put back");
    assert_eq!(std::fs::read_to_string(&file).unwrap(), "[SystemSettings]\r\nFullscreen = True\r\nResX=2560\r\nBorderless=Maybe\r\n");
    let other = Undo::IniKey { file: file.clone(), section: SECTION.into(), key: "ResX".into(), value: "1".into(), prior: None };
    assert!(undo(&file, &other).is_err(), "only the display-mode lines this fix sets");
    let _ = std::fs::remove_dir_all(root);
}
