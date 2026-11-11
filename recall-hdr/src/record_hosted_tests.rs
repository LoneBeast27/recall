use super::*;
use crate::config::Override;
use crate::rules::classify;
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
        launcher_game: false,
        store_game: false,
        hosted,
        window_class: String::new(),
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
    // Phone Link is a private app too (R4-detect-1): no candidate even with trace rights.
    assert_eq!(classify_checked(&phone, &cfg, true, true, || None), None);
    // An unknown Store app with trace rights is a candidate that must pass the check.
    let store = uwp("SomeStoreApp", "Some App", true);
    let checked = classify_checked(&store, &cfg, true, true, || None).expect("checked candidate");
    assert!(checked.needs_check);
    // The probe would not start for it: still no target.
    assert_eq!(classify_checked(&store, &cfg, true, false, || None), None);
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
fn a_plain_fullscreen_window_without_trace_rights_is_no_target() {
    // Official-proof ruling (2026-11-07): no verdict and no other proof = not recorded.
    let cfg = Config::default();
    assert_eq!(no_rights(&uwp("UnknownGame", "Unknown Game", false), &cfg, None), None, "no fallback");
    assert_eq!(no_rights(&uwp("UnknownGame", "Unknown Game", false), &cfg, Some(false)), None, "the vendor does not list it");
    let checked = classify_checked(&uwp("UnknownGame", "Unknown Game", false), &cfg, true, true, || None).expect("the check can run");
    assert!(checked.needs_check);
}

#[test]
fn hdr_rules_skip_the_fullscreen_default_for_a_resolved_identity() {
    let cfg = Config::default();
    // The HDR callers' own conversion (`app.rs`, `record_hdr.rs`).
    let fg = |ev: &WatchEvent| crate::app::foreground_of(ev);
    assert_eq!(classify(&fg(&uwp("PhoneExperienceHost", "Phone Link", true)), &cfg), AppKind::NonGame);
    // Phone Link is also a private app now: an unknown Store app tests the hosted rule alone.
    assert_eq!(classify(&fg(&uwp("SomeStoreApp", "Some App", true)), &cfg), AppKind::NonGame);
    assert!(hdr_fullscreen(&uwp("SomeStoreApp", "", false)), "plain: fullscreen flag kept");
    assert_ne!(classify(&fg(&uwp("Minecraft.Windows", "Minecraft", true)), &cfg), AppKind::NonGame, "known game");
    assert_ne!(classify(&fg(&uwp("UnknownGame", "x", false)), &cfg), AppKind::NonGame, "plain window: default class");
}
