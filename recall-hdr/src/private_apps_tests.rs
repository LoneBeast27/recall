use super::*;
use crate::config::{Config, Override};
use crate::record_composed::{composed_front, notice_step};
use crate::record_hosted::classify_checked;
use crate::render_verdict::{ComposedNotice, RenderStats};
use crate::rules::classify;
use crate::types::{AppKind, RuleKind};
use crate::watcher::WatchEvent;
use std::time::{Duration, Instant};

/// A plain (not frame-hosted) fullscreen window of `stem`.
fn plain(stem: &str) -> WatchEvent {
    WatchEvent {
        pid: 50,
        exe_stem: stem.into(),
        exe_name: format!("{stem}.exe"),
        title: format!("{stem} window"),
        fullscreen: true,
        fullscreen_on_target: true,
        steam_game: false,
        hosted: false,
        hwnd: 0x500,
    }
}

/// The app names as the watcher reports them (original case).
const APPS: &[&str] = &[
    "Zoom", "CptHost", "ms-teams", "msteams", "Teams", "webex", "CiscoCollabHost", "atmgr", "Skype", "lync", "slack",
    "WhatsApp", "WhatsApp.Root", "Telegram", "Signal", "Viber", "PhoneExperienceHost", "OUTLOOK", "olk", "thunderbird",
    "POWERPNT", "WINWORD", "EXCEL", "ONENOTE", "Acrobat", "AcroRd32", "Notion", "Obsidian", "Figma", "Spotify", "electron",
];

#[test]
fn every_listed_app_is_private_by_exact_stem() {
    for app in APPS {
        assert!(is_private(app), "{app}");
        assert!(is_private(&format!("{app}.exe")), "{app}.exe");
    }
    assert!(!is_private("SignalRgb"), "exact stem only");
    assert!(!is_private("zoomer"));
    assert!(!is_private(""));
}

#[test]
fn a_fullscreen_call_or_chat_app_is_no_target_with_or_without_trace_rights() {
    let cfg = Config::default();
    for app in APPS {
        let ev = plain(app);
        assert_eq!(classify_checked(&ev, &cfg, false, false, || None), None, "{app}: no trace rights");
        assert_eq!(classify_checked(&ev, &cfg, true, false, || None), None, "{app}: probe would not start");
        assert_eq!(classify_checked(&ev, &cfg, true, true, || None), None, "{app}: render check on");
        assert_eq!(classify_checked(&ev, &cfg, false, false, || Some(true)), None, "{app}: vendor-listed");
    }
}

#[test]
fn an_override_or_fullscreen_games_entry_opts_one_in() {
    let mut listed = Config::default();
    listed.detect.fullscreen_games.push("Zoom.exe".into());
    let zoom = classify_checked(&plain("Zoom"), &listed, false, false, || None).expect("fullscreen_games");
    assert!(!zoom.needs_check);
    let mut over = Config::default();
    over.overrides.push(Override { exe: "ms-teams".into(), kind: RuleKind::SdrGame, clamp: None });
    assert!(classify_checked(&plain("ms-teams"), &over, false, false, || None).is_some(), "game override");
    let mut none = Config::default();
    none.overrides.push(Override { exe: "slack".into(), kind: RuleKind::None, clamp: None });
    assert_eq!(classify_checked(&plain("slack"), &none, false, false, || None), None, "a none override stays none");
}

#[test]
fn a_real_game_is_unaffected() {
    let cfg = Config::default();
    let unknown = classify_checked(&plain("UnknownGame"), &cfg, false, false, || None).expect("today's rule");
    assert!(unknown.needs_check);
    let known = classify_checked(&plain("VALORANT-Win64-Shipping"), &cfg, false, false, || None).expect("known game");
    assert!(!known.needs_check);
}

#[test]
fn hdr_rules_give_a_call_app_no_game_class_unless_overridden() {
    let cfg = Config::default();
    for app in APPS {
        assert_eq!(classify(&crate::app::foreground_of(&plain(app)), &cfg), AppKind::NonGame, "{app}");
    }
    assert_ne!(classify(&crate::app::foreground_of(&plain("UnknownGame")), &cfg), AppKind::NonGame, "a game");
    let mut over = Config::default();
    over.overrides.push(Override { exe: "zoom".into(), kind: RuleKind::HdrGame, clamp: None });
    assert_eq!(classify(&crate::app::foreground_of(&plain("Zoom")), &over), AppKind::HdrGame, "override");
}

fn steady() -> Option<RenderStats> {
    Some(RenderStats { runtime_presents: 90, kernel_presents: 90, composed_frames: 90, ..RenderStats::default() })
}

#[test]
fn no_composed_notice_for_a_hosted_or_private_app() {
    // R4-detect-3: "add it as a game" never names a Store app or a chat app.
    assert_eq!(composed_front("PhoneExperienceHost", true, steady(), false), None);
    assert_eq!(composed_front("SomeStoreApp", true, steady(), false), None, "hosted");
    assert_eq!(composed_front("WhatsApp.Root", false, steady(), false), None, "private");
    assert_eq!(composed_front("video.ui", false, steady(), false), None, "Store media app");
    assert_eq!(composed_front("game", false, steady(), false), Some(true), "a plain composed game");
}

#[test]
fn the_composed_notice_waits_while_notices_are_off() {
    // R4-detect-2: with the overlay off nothing can show it; it must not be used up.
    let t0 = Instant::now();
    let mut n = ComposedNotice::default();
    let front = Some(((7, 70, "Game.exe"), true));
    for secs in [0, 10, 21, 30] {
        assert_eq!(notice_step(&mut n, front, false, t0 + Duration::from_secs(secs)), None, "overlay off");
    }
    // Notices back on: a fresh 20 s streak names it.
    assert_eq!(notice_step(&mut n, front, true, t0 + Duration::from_secs(31)), None);
    let named = notice_step(&mut n, front, true, t0 + Duration::from_secs(52));
    assert_eq!(named.as_deref(), Some("Game.exe"));
    assert_eq!(notice_step(&mut n, front, true, t0 + Duration::from_secs(60)), None, "once per exe");
}
