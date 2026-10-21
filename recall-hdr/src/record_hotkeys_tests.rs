use super::*;
use serde_json::json;

fn rec() -> RecordingCfg {
    RecordingCfg::default()
}

fn chord() -> HotkeysCfg {
    HotkeysCfg::default()
}

#[test]
fn defaults_are_two_direct_combos_plus_the_chord_leader() {
    let list = bindings(&rec(), &chord(), None);
    let pairs: Vec<(HotkeyAction, &str)> = list.iter().map(|b| (b.action, b.accel.as_str())).collect();
    assert_eq!(
        pairs,
        vec![(HotkeyAction::Save { seconds: 0 }, "Alt+N"), (HotkeyAction::Mic, "Alt+M"), (HotkeyAction::Leader, "Alt+Backquote")]
    );
    let mut off = chord();
    off.advanced_enabled = false;
    assert!(!bindings(&rec(), &off, None).iter().any(|b| b.action == HotkeyAction::Leader), "disabled flag drops the leader");
}

#[test]
fn defaults_avoid_combos_games_and_overlays_use() {
    let mut defaults: Vec<String> = bindings(&rec(), &chord(), Some(&crate::config::HotkeyCfg::default().toggle_hdr))
        .into_iter()
        .map(|b| b.accel)
        .collect();
    defaults.extend(["Alt+1", "Alt+2", "Alt+3", "Alt+4"].map(String::from)); // the chord's Alt-held variants
    for accel in &defaults {
        assert!(!is_gamer_combo(accel), "default '{accel}' is a combo games use");
    }
    for taken in ["Alt+R", "Alt+Z", "alt+f9", "Ctrl+Shift+S", "F12", "Shift+Tab", "Win+Alt+R", "Win+Alt+PrintScreen", "Ctrl+Alt+R"] {
        assert!(is_gamer_combo(taken), "'{taken}' must be on the never-default list");
    }
}

#[test]
fn defaults_do_not_conflict_with_each_other_or_the_hdr_key() {
    let (keep, warnings) = split_conflicts(bindings(&rec(), &chord(), Some("Ctrl+Alt+H")), None);
    assert!(warnings.is_empty(), "{warnings:?}");
    assert_eq!(keep.len(), 4);
    assert!(validate(&rec(), &chord()).is_ok());
}

#[test]
fn direct_combos_stay_configurable() {
    let mut r = rec();
    r.bookmark_hotkey = "Ctrl+F7".into();
    r.screenshot_hotkey = "Ctrl+F8".into();
    r.record_hotkey = "Ctrl+F6".into();
    r.save_hotkeys.push(SaveHotkey { hotkey: "Ctrl+F5".into(), seconds: 30 });
    let actions: Vec<HotkeyAction> = bindings(&r, &chord(), None).iter().map(|b| b.action).collect();
    for a in [HotkeyAction::Bookmark, HotkeyAction::Screenshot, HotkeyAction::Record, HotkeyAction::Save { seconds: 30 }] {
        assert!(actions.contains(&a), "{a:?}");
    }
    assert!(validate(&r, &chord()).is_ok());
}

#[test]
fn conflicts_and_bad_accelerators_become_warnings_first_owner_wins() {
    let mut r = rec();
    r.bookmark_hotkey = "alt+n".into(); // same chord as the full-buffer save
    r.screenshot_hotkey = "Ctrl+Nonsense".into();
    r.record_hotkey = "Ctrl+F6".into();
    let (keep, warnings) = split_conflicts(bindings(&r, &chord(), Some("Ctrl+F6")), None); // HDR toggle collides with record
    let kept: Vec<HotkeyAction> = keep.iter().map(|(b, _)| b.action).collect();
    assert!(kept.contains(&HotkeyAction::Save { seconds: 0 }));
    assert!(!kept.contains(&HotkeyAction::Bookmark));
    assert!(!kept.contains(&HotkeyAction::Screenshot));
    assert!(kept.contains(&HotkeyAction::Record) && !kept.contains(&HotkeyAction::ToggleHdr));
    assert_eq!(warnings.len(), 3, "{warnings:?}");
    assert!(warnings[0].contains("bookmark hotkey 'alt+n' conflicts with the save (full buffer) hotkey"), "{}", warnings[0]);
    assert!(warnings[1].contains("screenshot hotkey is not registered"), "{}", warnings[1]);
    assert!(warnings[2].contains("HDR toggle"), "{}", warnings[2]);
    assert!(validate(&r, &chord()).unwrap_err().contains("must differ"));
    let mut dup = rec();
    dup.record_hotkey = "Alt+M".into();
    assert_eq!(
        validate(&dup, &chord()).unwrap_err(),
        "recording hotkeys must differ: 'Alt+M' is used by both microphone and long recording"
    );
    let mut lead = chord();
    lead.leader = "Alt+N".into();
    assert!(validate(&rec(), &lead).unwrap_err().contains("advanced shortcut leader"));
}

#[test]
fn chord_settings_are_bounded() {
    for (patch, needle) in [
        (HotkeysCfg { leader: " ".into(), ..chord() }, "hotkeys.leader"),
        (HotkeysCfg { save_seconds: 0, ..chord() }, "hotkeys.save_seconds"),
        (HotkeysCfg { window_ms: 100, ..chord() }, "hotkeys.window_ms"),
        (HotkeysCfg { leader: "Alt+Nonsense".into(), ..chord() }, "hotkeys.leader"),
    ] {
        let err = validate(&rec(), &patch).unwrap_err();
        assert!(err.contains(needle), "{err}");
    }
    let off = HotkeysCfg { advanced_enabled: false, leader: String::new(), ..chord() };
    assert!(validate(&rec(), &off).is_ok(), "no leader needed while disabled");
}

#[test]
fn legacy_mirror_and_full_buffer_moves() {
    let list = vec![SaveHotkey { hotkey: "Alt+N".into(), seconds: 0 }, SaveHotkey { hotkey: "Ctrl+F5".into(), seconds: 30 }];
    assert_eq!(legacy_save_hotkey(&list), "Alt+N");
    assert_eq!(legacy_save_hotkey(&[SaveHotkey { hotkey: "F1".into(), seconds: 9 }]), "F1", "no full preset -> first");
    assert_eq!(legacy_save_hotkey(&[]), "");
    let moved = with_full_buffer_hotkey(&list, "F9");
    assert_eq!(moved[0].hotkey, "F9");
    assert_eq!(moved[1], list[1]);
    let inserted = with_full_buffer_hotkey(&list[1..], "F9");
    assert_eq!(inserted[0], SaveHotkey { hotkey: "F9".into(), seconds: 0 });
}

#[test]
fn migration_keeps_the_legacy_key_as_the_full_buffer_save() {
    let mut r = rec();
    r.save_hotkey = "F9".into();
    assert_eq!(migrated(&r), vec![SaveHotkey { hotkey: "F9".into(), seconds: 0 }]);
    r.save_hotkey = "".into();
    assert_eq!(migrated(&r)[0].hotkey, "Alt+N", "empty legacy key -> default");
}

#[test]
fn legacy_patch_rewrite_shapes() {
    let base = rec();
    let p = rewrite_legacy_patch(&base, &json!({"recording":{"save_hotkey":"F9","buffer_secs":90}}));
    assert_eq!(p, json!({"recording":{"buffer_secs":90,"save_hotkeys":[{"hotkey":"F9","seconds":0}]}}));
    let p = rewrite_legacy_patch(&base, &json!({"recording":{"save_hotkey":"F9","save_hotkeys":[]}}));
    assert_eq!(p, json!({"recording":{"save_hotkeys":[]}}), "the list wins");
    let untouched = json!({"overlay":{"enabled":true}});
    assert_eq!(rewrite_legacy_patch(&base, &untouched), untouched);
    let bad = json!({"recording":{"save_hotkey":5}});
    assert_eq!(rewrite_legacy_patch(&base, &bad), bad, "non-strings are left for the parser to reject");
}
