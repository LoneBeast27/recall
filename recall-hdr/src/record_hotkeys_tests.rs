use super::*;
use serde_json::json;

fn rec() -> RecordingCfg {
    RecordingCfg::default()
}

#[test]
fn defaults_are_one_direct_alt_combo_per_action() {
    let list = bindings(&rec(), None);
    let pairs: Vec<(HotkeyAction, &str)> = list.iter().map(|b| (b.action, b.accel.as_str())).collect();
    assert_eq!(
        pairs,
        vec![
            (HotkeyAction::Save { seconds: 0 }, "Alt+N"),
            (HotkeyAction::Mic, "Alt+M"),
            (HotkeyAction::Bookmark, "Alt+B"),
            (HotkeyAction::Screenshot, "Alt+S"),
            (HotkeyAction::Record, "Alt+L"),
            (HotkeyAction::Stats, "Alt+P"),
        ]
    );
}

#[test]
fn defaults_avoid_combos_games_and_overlays_use() {
    let defaults: Vec<String> =
        bindings(&rec(), Some(&crate::config::HotkeyCfg::default().toggle_hdr)).into_iter().map(|b| b.accel).collect();
    for accel in &defaults {
        assert!(!is_gamer_combo(accel), "default '{accel}' is a combo games use");
    }
    for taken in ["Alt+R", "Alt+Z", "alt+f9", "Ctrl+Shift+S", "F12", "Shift+Tab", "Win+Alt+R", "Win+Alt+PrintScreen", "Ctrl+Alt+R"] {
        assert!(is_gamer_combo(taken), "'{taken}' must be on the never-default list");
    }
}

#[test]
fn defaults_do_not_conflict_with_each_other_or_the_hdr_key() {
    let (keep, warnings) = split_conflicts(bindings(&rec(), Some("Ctrl+Alt+H")));
    assert!(warnings.is_empty(), "{warnings:?}");
    assert_eq!(keep.len(), 7);
    assert!(validate(&rec()).is_ok());
}

#[test]
fn direct_combos_stay_configurable() {
    let mut r = rec();
    r.bookmark_hotkey = "Ctrl+F7".into();
    r.screenshot_hotkey = "Ctrl+F8".into();
    r.record_hotkey = "Ctrl+F6".into();
    r.stats_hotkey = "Ctrl+F4".into();
    r.save_hotkeys.push(SaveHotkey { hotkey: "Ctrl+F5".into(), seconds: 30 });
    let list = bindings(&r, None);
    for (a, accel) in [
        (HotkeyAction::Bookmark, "Ctrl+F7"),
        (HotkeyAction::Screenshot, "Ctrl+F8"),
        (HotkeyAction::Record, "Ctrl+F6"),
        (HotkeyAction::Stats, "Ctrl+F4"),
        (HotkeyAction::Save { seconds: 30 }, "Ctrl+F5"),
    ] {
        assert!(list.iter().any(|b| b.action == a && b.accel == accel), "{a:?}");
    }
    assert!(validate(&r).is_ok());
}

#[test]
fn optional_combos_can_be_turned_off() {
    let mut r = rec();
    for slot in [&mut r.bookmark_hotkey, &mut r.screenshot_hotkey, &mut r.record_hotkey, &mut r.stats_hotkey] {
        slot.clear();
    }
    let actions: Vec<HotkeyAction> = bindings(&r, None).iter().map(|b| b.action).collect();
    assert_eq!(actions, vec![HotkeyAction::Save { seconds: 0 }, HotkeyAction::Mic]);
    assert!(validate(&r).is_ok());
}

#[test]
fn conflicts_and_bad_accelerators_become_warnings_first_owner_wins() {
    let mut r = rec();
    r.bookmark_hotkey = "alt+n".into(); // same chord as the full-buffer save
    r.screenshot_hotkey = "Ctrl+Nonsense".into();
    r.record_hotkey = "Ctrl+F6".into();
    let (keep, warnings) = split_conflicts(bindings(&r, Some("Ctrl+F6"))); // HDR toggle collides with record
    let kept: Vec<HotkeyAction> = keep.iter().map(|(b, _)| b.action).collect();
    assert!(kept.contains(&HotkeyAction::Save { seconds: 0 }));
    assert!(!kept.contains(&HotkeyAction::Bookmark));
    assert!(!kept.contains(&HotkeyAction::Screenshot));
    assert!(kept.contains(&HotkeyAction::Record) && !kept.contains(&HotkeyAction::ToggleHdr));
    assert_eq!(warnings.len(), 3, "{warnings:?}");
    assert!(warnings[0].contains("bookmark hotkey 'alt+n' conflicts with the save (full buffer) hotkey"), "{}", warnings[0]);
    assert!(warnings[1].contains("screenshot hotkey is not registered"), "{}", warnings[1]);
    assert!(warnings[2].contains("HDR toggle"), "{}", warnings[2]);
    assert!(validate(&r).unwrap_err().contains("must differ"));
    let mut dup = rec();
    dup.record_hotkey = "Alt+M".into();
    assert_eq!(
        validate(&dup).unwrap_err(),
        "recording hotkeys must differ: 'Alt+M' is used by both microphone and long recording"
    );
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

/// A config saved in the two-step-shortcut era: `[hotkeys]` present, the
/// direct bookmark / screenshot / record keys written as "" (their old
/// default), no stats key.
const CHORD_ERA: &str = "[recording]\nbookmark_hotkey = \"\"\nscreenshot_hotkey = \"\"\nrecord_hotkey = \"Ctrl+F6\"\n\n[hotkeys]\nadvanced_enabled = true\nleader = \"Alt+Backquote\"\n";

fn load(text: &str) -> RecordingCfg {
    let mut r: RecordingCfg = toml::from_str::<toml::Value>(text)
        .unwrap()
        .get("recording")
        .map(|v| v.clone().try_into().unwrap())
        .unwrap_or_default();
    migrate_loaded(&mut r, text);
    r
}

#[test]
fn a_chord_era_config_gets_the_direct_defaults_it_left_empty() {
    let r = load(CHORD_ERA);
    assert_eq!(r.bookmark_hotkey, "Alt+B");
    assert_eq!(r.screenshot_hotkey, "Alt+S");
    assert_eq!(r.record_hotkey, "Ctrl+F6", "a key the user set is kept");
    assert_eq!(r.stats_hotkey, "Alt+P");
    assert!(validate(&r).is_ok());
}

#[test]
fn chord_era_defaults_never_take_a_combo_already_in_use() {
    let text = CHORD_ERA.replace("record_hotkey = \"Ctrl+F6\"", "record_hotkey = \"Alt+B\"");
    let r = load(&text);
    assert_eq!(r.record_hotkey, "Alt+B");
    assert_eq!(r.bookmark_hotkey, "", "Alt+B is the record key's, so bookmark stays off");
    assert!(validate(&r).is_ok());
}

#[test]
fn a_new_era_config_keeps_its_empty_keys_off() {
    let text = "[recording]\nbookmark_hotkey = \"\"\nstats_hotkey = \"\"\n";
    let r = load(text);
    assert_eq!((r.bookmark_hotkey.as_str(), r.stats_hotkey.as_str()), ("", ""), "no [hotkeys]: the user turned them off");
    assert_eq!(r.screenshot_hotkey, "Alt+S", "a missing key takes its default");
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
