use super::*;
use crate::config::Override;
use crate::rules::{classify, Foreground};
use crate::types::{AppKind, RuleKind};

/// A fullscreen UWP app resolved from its frame (G6) when `hosted`.
fn uwp(stem: &str, title: &str, hosted: bool) -> WatchEvent {
    WatchEvent {
        pid: 40,
        exe_stem: stem.into(),
        exe_name: format!("{stem}.exe"),
        title: title.into(),
        fullscreen: true,
        fullscreen_on_target: true,
        steam_game: false,
        hosted,
        hwnd: 0x400,
    }
}

/// No trace rights: the render check is off for every pid.
fn no_rights(ev: &WatchEvent, cfg: &Config, vendor: Option<bool>) -> Option<Classified> {
    classify_checked(ev, cfg, false, false, || vendor)
}

#[test]
fn phone_link_fullscreen_without_trace_rights_is_no_target() {
    let cfg = Config::default();
    let phone = uwp("PhoneExperienceHost", "Phone Link", true);
    assert_eq!(no_rights(&phone, &cfg, None), None, "fallback never applies to a resolved identity");
    assert_eq!(no_rights(&phone, &cfg, Some(false)), None, "the vendor does not list it");
    // With trace rights it is a candidate that must pass the check.
    let checked = classify_checked(&phone, &cfg, true, true, || None).expect("checked candidate");
    assert!(checked.needs_check);
    // The probe would not start for it: still no target.
    assert_eq!(classify_checked(&phone, &cfg, true, false, || None), None);
}

#[test]
fn an_unknown_uwp_app_without_trace_rights_is_no_target() {
    let cfg = Config::default();
    for (stem, title) in [("WhatsApp", "WhatsApp"), ("Clipchamp", "Clipchamp - Video Editor"), ("SomeStoreApp", "")] {
        assert_eq!(no_rights(&uwp(stem, title, true), &cfg, None), None, "{stem}");
    }
}

#[test]
fn a_known_game_uwp_override_or_vendor_listed_one_still_is() {
    let cfg = Config::default();
    let mc = no_rights(&uwp("Minecraft.Windows", "Minecraft", true), &cfg, None).expect("known game");
    assert!(!mc.needs_check, "known game: 300 ms rule");
    let listed = no_rights(&uwp("SolitaireStore", "Solitaire", true), &cfg, Some(true)).expect("vendor-listed");
    assert!(!listed.needs_check);
    let mut over = Config::default();
    over.overrides.push(Override { exe: "Clipchamp".into(), kind: RuleKind::SdrGame, clamp: None });
    assert!(no_rights(&uwp("Clipchamp", "", true), &over, None).is_some(), "a user override");
    let mut listed_cfg = Config::default();
    listed_cfg.detect.fullscreen_games.push("WhatsApp.exe".into());
    assert!(no_rights(&uwp("WhatsApp", "", true), &listed_cfg, None).is_some(), "detect.fullscreen_games");
}

#[test]
fn a_plain_fullscreen_window_keeps_todays_rule_without_trace_rights() {
    let cfg = Config::default();
    let plain = no_rights(&uwp("UnknownGame", "Unknown Game", false), &cfg, None).expect("fallback");
    assert!(plain.needs_check, "the check would run if it could");
    assert!(commits_without_probe(false));
}

#[test]
fn a_hosted_candidate_with_no_probe_does_not_commit() {
    assert!(!commits_without_probe(true));
}

#[test]
fn hdr_rules_skip_the_fullscreen_default_for_a_resolved_identity() {
    let cfg = Config::default();
    let fg = |ev: &WatchEvent| Foreground {
        exe_stem: ev.exe_stem.to_ascii_lowercase(),
        title: ev.title.clone(),
        fullscreen_on_target: hdr_fullscreen(ev),
        pid: ev.pid,
    };
    assert_eq!(classify(&fg(&uwp("PhoneExperienceHost", "Phone Link", true)), &cfg), AppKind::NonGame);
    assert_ne!(classify(&fg(&uwp("Minecraft.Windows", "Minecraft", true)), &cfg), AppKind::NonGame, "known game");
    assert_ne!(classify(&fg(&uwp("UnknownGame", "x", false)), &cfg), AppKind::NonGame, "plain window: default class");
}
