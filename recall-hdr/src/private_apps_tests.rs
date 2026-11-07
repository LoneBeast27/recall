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
        launcher_game: false,
        hosted: false,
        hwnd: 0x500,
    }
}

/// The app names as the watcher reports them (original case).
const APPS: &[&str] = &[
    "Zoom", "CptHost", "ms-teams", "msteams", "Teams", "webex", "CiscoCollabHost", "atmgr", "Skype", "lync", "slack",
    "WhatsApp", "WhatsApp.Root", "Telegram", "Signal", "Viber", "PhoneExperienceHost", "OUTLOOK", "olk", "thunderbird",
    "POWERPNT", "WINWORD", "EXCEL", "ONENOTE", "Acrobat", "AcroRd32", "Notion", "Obsidian", "Figma", "Spotify", "electron",
    // R5-detect-1: more chat apps, media players / video apps, office suites, music apps.
    "Messenger", "WeChat", "Weixin", "LINE", "Element", "KakaoTalk", "Mattermost", "Rocket.Chat", "Wire", "Zulip",
    "Kodi", "Plex", "Plex HTPC", "Stremio", "stremio-shell-ng", "JellyfinMediaPlayer", "mpc-be", "mpc-be64",
    "PotPlayer", "PotPlayer64", "PotPlayerMini", "smplayer", "mpvnet", "PrimeVideo", "DisneyPlus",
    "soffice", "simpress", "swriter", "scalc", "wps", "wpp", "DesktopEditors",
    "foobar2000", "TIDAL", "YouTube Music Desktop App", "iTunes", "AppleMusic",
];

/// One representative per category added in R5-detect-1.
const NEW_CATEGORIES: &[&str] = &["KakaoTalk", "Kodi", "soffice", "TIDAL"];

#[test]
fn a_new_category_representative_records_only_with_an_opt_in() {
    for app in NEW_CATEGORIES {
        let ev = plain(app);
        assert_eq!(classify_checked(&ev, &Config::default(), false, false, || None), None, "{app}: no opt-in");
        assert_eq!(classify_checked(&ev, &Config::default(), true, true, || None), None, "{app}: render check on");
        let mut over = Config::default();
        over.overrides.push(Override { exe: (*app).into(), kind: RuleKind::SdrGame, clamp: None });
        assert!(classify_checked(&ev, &over, false, false, || None).is_some(), "{app}: game override");
        let mut listed = Config::default();
        listed.detect.fullscreen_games.push(format!("{app}.exe"));
        assert!(classify_checked(&ev, &listed, false, false, || None).is_some(), "{app}: fullscreen_games");
    }
}

#[test]
fn a_steam_library_game_shipping_a_listed_exe_name_still_records() {
    // R5-detect-2: an indie Steam game can ship a stock electron.exe (or be named Element.exe).
    let cfg = Config::default();
    for stem in ["electron", "Element"] {
        let ev = WatchEvent { steam_game: true, ..plain(stem) };
        let t = crate::record_target::classify_event(&ev, &cfg).unwrap_or_else(|| panic!("{stem}: Steam library"));
        assert_eq!(t.exe, ev.exe_name);
        let windowed = WatchEvent { fullscreen: false, fullscreen_on_target: false, ..ev.clone() };
        assert!(crate::record_target::classify_event(&windowed, &cfg).is_some(), "{stem}: windowed Steam game");
    }
    assert_eq!(crate::record_target::classify_event(&plain("electron"), &cfg), None, "outside Steam: private");
}

#[test]
fn steam_evidence_counts_on_the_adoption_and_follow_routes() {
    // R6-detect-3: dropping `game_evidence(ev)` on either route must fail a test.
    let cfg = Config::default();
    let steam = WatchEvent { steam_game: true, fullscreen: false, fullscreen_on_target: false, ..plain("electron") };
    let outside = WatchEvent { steam_game: false, ..steam.clone() };
    let adopt = crate::record_target_gate::adopted_candidate;
    assert!(adopt(&steam, &cfg, (1920, 1080)).is_some(), "adoption: Steam game");
    assert!(adopt(&outside, &cfg, (1920, 1080)).is_none(), "adoption: private outside Steam");
    assert!(crate::record_target::followable(&steam, &cfg), "follow: Steam game");
    assert!(!crate::record_target::followable(&outside, &cfg), "follow: private outside Steam");
}

#[test]
fn hdr_rules_let_steam_evidence_beat_the_private_list() {
    // R6-detect-1: a Steam game called Element.exe records, so HDR treats it as a game too.
    let cfg = Config::default();
    let steam = WatchEvent { steam_game: true, ..plain("Element") };
    assert_ne!(classify(&crate::app::foreground_of(&steam), &cfg), AppKind::NonGame, "Steam: game class");
    assert_eq!(classify(&crate::app::foreground_of(&plain("Element")), &cfg), AppKind::NonGame, "outside Steam");
}

#[test]
fn hdr_rules_honour_a_fullscreen_games_opt_in() {
    // R5-detect-3: a `detect.fullscreen_games` entry opts a private app in for the HDR rules too.
    let mut cfg = Config::default();
    cfg.detect.fullscreen_games.push("Zoom.exe".into());
    let kind = classify(&crate::app::foreground_of(&plain("Zoom")), &cfg);
    assert_eq!(kind, crate::rules::classify(&crate::app::foreground_of(&plain("UnknownGame")), &cfg), "game class");
    assert_ne!(kind, AppKind::NonGame);
    let windowed = WatchEvent { fullscreen: false, fullscreen_on_target: false, ..plain("Zoom") };
    assert_eq!(classify(&crate::app::foreground_of(&windowed), &cfg), AppKind::NonGame, "windowed stays NonGame");
}

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
