//! Official proof only (the maintainer 2026-11-07 07:46 UTC: "use official proof +
//! support for manual application registering"): an unknown fullscreen exe
//! is recorded only with proof that it is a game, or after the user registers
//! it. Names here are made up (no real device or app data).

use serde_json::json;

use crate::config::Config;
use crate::game_library::{game_library, Library};
use crate::record_config::apply_patch;
use crate::record_hosted::classify_checked;
use crate::record_target::{classify_event, followable};
use crate::watcher::WatchEvent;

/// A plain window of `stem` (no library, not resolved by Recall).
fn window(stem: &str, fullscreen: bool) -> WatchEvent {
    WatchEvent {
        pid: 90,
        exe_stem: stem.into(),
        exe_name: format!("{stem}.exe"),
        title: stem.into(),
        fullscreen,
        fullscreen_on_target: fullscreen,
        steam_game: false,
        launcher_game: false,
        store_game: false,
        hosted: false,
        hwnd: 0x900,
    }
}

/// A target with no trace rights, the vendor not listing it.
fn no_rights(ev: &WatchEvent, cfg: &Config) -> bool {
    classify_checked(ev, cfg, false, false, || Some(false)).is_some()
}

#[test]
fn an_unknown_fullscreen_exe_with_no_proof_and_no_trace_rights_is_not_a_target() {
    let cfg = Config::default();
    let ev = window("MysteryApp", true);
    assert!(!no_rights(&ev, &cfg), "no trace rights, vendor says no");
    assert!(classify_checked(&ev, &cfg, false, false, || None).is_none(), "vendor answer pending");
    assert!(classify_checked(&ev, &cfg, true, false, || Some(false)).is_none(), "the check could not start for it");
    let checked = classify_checked(&ev, &cfg, true, true, || Some(false)).expect("the check can run: it decides");
    assert!(checked.needs_check, "only a game verdict commits it");
    assert!(classify_checked(&ev, &cfg, false, false, || Some(true)).is_some_and(|c| !c.needs_check), "NVIDIA lists it");
}

#[test]
fn a_manual_add_makes_it_a_target_and_removing_the_entry_stops_it() {
    let base = Config::default();
    // The patch shape Settings -> Games sends (recall-native `game_apps`); G9: both spellings match.
    for exe in ["MysteryApp.exe", "mysteryapp"] {
        let added = apply_patch(&base, &json!({"overrides": [{"exe": exe, "kind": "sdr_game"}]})).expect("valid patch").config;
        let fs = classify_checked(&window("MysteryApp", true), &added, false, false, || Some(false)).expect(exe);
        assert!(!fs.needs_check, "{exe}: a registration needs no render check");
        assert!(classify_event(&window("MysteryApp", false), &added).is_some(), "{exe}: windowed too");
        let removed = apply_patch(&added, &json!({"overrides": []})).expect("valid patch").config;
        assert!(!no_rights(&window("MysteryApp", true), &removed), "{exe}: removed = no proof again");
    }
}

#[test]
fn a_never_record_entry_blocks_even_steam_evidence() {
    let never = apply_patch(&Config::default(), &json!({"overrides": [{"exe": "SomeGame.exe", "kind": "none"}]})).expect("valid").config;
    let steam = WatchEvent { steam_game: true, ..window("SomeGame", true) };
    assert!(classify_event(&steam, &Config::default()).is_some(), "Steam evidence alone records it");
    assert!(classify_event(&steam, &never).is_none(), "fullscreen Steam game, Never record");
    assert!(classify_event(&WatchEvent { fullscreen: false, ..steam.clone() }, &never).is_none(), "windowed");
    assert!(!followable(&steam, &never), "follow route");
    let known = apply_patch(&Config::default(), &json!({"overrides": [{"exe": "valorant-win64-shipping", "kind": "none"}]})).expect("valid").config;
    assert!(classify_event(&window("VALORANT-Win64-Shipping", true), &known).is_none(), "a known game too");
    let listed = apply_patch(&never, &json!({"detect": {"fullscreen_games": ["SomeGame"]}})).expect("valid").config;
    assert!(classify_event(&steam, &listed).is_none(), "Never record beats fullscreen_games");
}

#[test]
fn fullscreen_discord_is_a_game_only_through_fullscreen_games() {
    // The maintainer 2026-11-05: fullscreen Discord stays a game via [detect] fullscreen_games = ["Discord"].
    let cfg = Config::default();
    assert_eq!(cfg.detect.fullscreen_games, vec!["Discord".to_string()]);
    let fs = classify_checked(&window("Discord", true), &cfg, false, false, || Some(false)).expect("fullscreen Discord, no trace rights");
    assert!(!fs.needs_check);
    assert!(classify_event(&window("Discord", false), &cfg).is_none(), "windowed Discord is not");
    let mut off = cfg.clone();
    off.detect.fullscreen_games.clear();
    assert!(classify_event(&window("Discord", true), &off).is_none(), "without the entry it is denied");
    for chat in ["Element", "ChatterBoxFixture"] {
        assert!(!no_rights(&window(chat, true), &cfg), "{chat}: another chat app fullscreen is not");
        assert!(classify_event(&window(chat, false), &cfg).is_none(), "{chat}: windowed");
    }
}

/// A made-up install tree under the temp folder; removed on drop.
struct Tree(std::path::PathBuf);

impl Tree {
    fn new(name: &str) -> Tree {
        let dir = std::env::temp_dir().join(format!("recall-proof-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("temp dir");
        Tree(dir)
    }
    /// Writes `MicrosoftGame.config` into `rel` and returns that folder.
    fn package(&self, rel: &str) -> std::path::PathBuf {
        let dir = self.0.join(rel);
        std::fs::create_dir_all(&dir).expect("package dir");
        std::fs::write(dir.join("MicrosoftGame.Config"), "<Game configVersion=\"1\"/>").expect("config");
        dir
    }
    fn path(&self, rel: &str) -> String {
        self.0.join(rel).to_string_lossy().into_owned()
    }
}

impl Drop for Tree {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[test]
fn an_xbox_or_store_package_with_a_game_config_is_official_proof() {
    let t = Tree::new("store");
    t.package("XboxGames\\Fixture Game\\Content");
    t.package("WindowsApps\\Fixture.Game_1.0.0.0_x64__abc123");
    for exe in [
        "XboxGames\\Fixture Game\\Content\\FixtureGame.exe",
        "XboxGames\\Fixture Game\\Content\\bin\\x64\\FixtureGame-Win64.exe",
        "WindowsApps\\Fixture.Game_1.0.0.0_x64__abc123\\Game.exe",
    ] {
        assert_ne!(game_library(&t.path(exe)), Library::None, "{exe}");
    }
    for exe in [
        "Tools\\SomeTool\\tool.exe",
        "XboxGames\\Fixture Game\\Content\\_CommonRedist\\vc.exe",
        "WindowsApps\\Other.App_1.0_x64__zz\\App.exe",
    ] {
        assert_eq!(game_library(&t.path(exe)), Library::None, "{exe}");
    }
}

#[test]
fn a_store_package_proves_a_fullscreen_window_only_and_not_against_the_private_list() {
    let cfg = Config::default();
    let game = WatchEvent { store_game: true, ..window("FixtureGame", true) };
    let fs = classify_checked(&game, &cfg, false, false, || Some(false)).expect("Store package, no trace rights");
    assert!(!fs.needs_check, "official proof: no render check");
    assert!(classify_event(&WatchEvent { fullscreen: false, ..game.clone() }, &cfg).is_none(), "windowed: a launcher's package is no game");
    let chat = WatchEvent { store_game: true, ..window("Element", true) };
    assert!(classify_event(&chat, &cfg).is_none(), "the private list beats Store-package evidence");
    let hosted = WatchEvent { hosted: true, ..game.clone() };
    assert!(classify_checked(&hosted, &cfg, false, false, || None).is_some(), "a resolved UWP identity in a game package");
}

#[test]
fn no_verdict_is_named_by_the_g4_notice_but_never_recorded() {
    use crate::record_hosted::{check_proof, Checked};
    use crate::record_render::unproven_front;
    let cfg = Config::default();
    let ev = window("MysteryApp", true);
    assert_eq!(check_proof(&ev, &cfg, false, || Some(false)), Checked::NoVerdict);
    assert_eq!(check_proof(&window("Element", true), &cfg, false, || Some(false)), Checked::No, "private: its own path");
    assert_eq!(check_proof(&window("MysteryApp", false), &cfg, false, || Some(false)), Checked::No, "windowed: no candidate");
    assert_eq!(unproven_front(&ev, false, false, false), Some(true), "the 20 s clock runs");
    assert_eq!(unproven_front(&ev, true, false, false), None, "paused");
    assert_eq!(unproven_front(&ev, false, true, false), None, "already recorded");
    assert_eq!(unproven_front(&ev, false, false, true), None, "the desktop capture has it");
    assert_eq!(unproven_front(&WatchEvent { hosted: true, ..ev.clone() }, false, false, false), None, "a resolved Store app");
    assert_eq!(unproven_front(&window("MysteryApp", false), false, false, false), None, "windowed");
    // The notice text is the SPEC's.
    let (title, detail) = crate::record_composed::notice_text("MysteryApp.exe");
    assert_eq!(format!("{title}: {detail}"), "Not recording MysteryApp.exe: add it as a game in Settings");
    // Once per exe, after 20 s in front.
    let mut notice = crate::render_verdict::ComposedNotice::default();
    let t0 = std::time::Instant::now();
    let seen = |n: &mut crate::render_verdict::ComposedNotice, secs| n.observe_steady(Some(((90, 0x900, "MysteryApp.exe"), true)), t0 + std::time::Duration::from_secs(secs));
    assert_eq!(seen(&mut notice, 0), None);
    assert_eq!(seen(&mut notice, 21).as_deref(), Some("MysteryApp.exe"));
    assert_eq!(seen(&mut notice, 60), None, "once per exe");
}
