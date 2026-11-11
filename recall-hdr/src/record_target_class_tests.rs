//! Classification tests for `record_target` (known games, screensavers, Store
//! apps, the Minecraft server GUI); split from `record_target_tests.rs`.

use super::*;

#[test]
fn known_game_windowed_is_a_candidate() {
    // G10: Minecraft Java starts in an 854x480 window; it is a known game.
    let cfg = Config::default();
    let mc = ev("javaw", "Minecraft 1.21.1", 5, 0x5, false);
    let t = classify_event(&mc, &cfg).expect("a windowed known game is a candidate");
    assert_eq!((t.pid, t.hwnd), (5, 0x5));
    assert!(skips_render_check(&mc, &cfg));
    assert!(classify_event(&ev("javaw", "IntelliJ IDEA", 6, 0x6, false), &cfg).is_none(), "an unknown windowed Java app is not");
    let mut off = cfg.clone();
    off.overrides.push(Override { exe: "javaw".into(), kind: RuleKind::None, clamp: None });
    assert!(classify_event(&mc, &off).is_none(), "a none override still wins");
}

#[test]
fn a_modpack_titled_java_game_window_is_a_known_game() {
    // Modpacks, Lunar and Feather rename the title; LWJGL's window class stays.
    let cfg = Config::default();
    let pack = WatchEvent { window_class: "GLFW30".into(), ..ev("javaw", "All the Mods 10 - ATM10", 5, 0x5, false) };
    assert!(classify_event(&pack, &cfg).is_some(), "windowed");
    assert!(skips_render_check(&pack, &cfg), "no render check, like the Minecraft title");
    let launcher = WatchEvent { window_class: "SunAwtFrame".into(), ..ev("javaw", "ATLauncher", 6, 0x6, false) };
    assert!(classify_event(&launcher, &cfg).is_none(), "a Swing launcher window is not");
    let mut off = cfg.clone();
    off.overrides.push(Override { exe: "javaw".into(), kind: RuleKind::None, clamp: None });
    assert!(classify_event(&pack, &off).is_none(), "a none override still wins");
}

#[test]
fn every_minecraft_java_window_is_named_minecraft() {
    // One game folder for vanilla, every version and every modpack.
    let cfg = Config::default();
    let pack = WatchEvent { window_class: "GLFW30".into(), minecraft_java: true, ..ev("javaw", "All the Mods 10 - ATM10", 5, 0x5, false) };
    assert_eq!(classify_event(&pack, &cfg).expect("a game").name, crate::record_target::MINECRAFT);
    let other = WatchEvent { window_class: "GLFW30".into(), ..ev("java", "Starsector 0.97a", 6, 0x6, false) };
    assert_eq!(classify_event(&other, &cfg).expect("an LWJGL game").name, "Starsector 0.97a", "other Java games keep their title");
}

#[test]
fn the_minecraft_server_gui_is_never_a_target() {
    let cfg = Config::default();
    assert!(classify_event(&ev("javaw", "Minecraft server", 6, 0x6, false), &cfg).is_none(), "windowed");
    assert!(classify_event(&ev("java", "Minecraft server", 6, 0x6, true), &cfg).is_none(), "even fullscreen");
    assert!(classify_event(&ev("javaw", "Minecraft 1.21.1", 6, 0x6, false), &cfg).is_some(), "the game client");
}

#[test]
fn screensaver_is_never_a_target() {
    // G11: a .scr runs fullscreen and topmost.
    let cfg = Config::default();
    for name in ["Bubbles.scr", "ribbons.SCR", "Mystify.Scr"] {
        let stem = name.rsplit_once('.').unwrap().0;
        let e = WatchEvent { exe_name: name.into(), ..ev(stem, stem, 8, 0x8, true) };
        assert!(classify_event(&e, &cfg).is_none(), "{name}");
        let steam = WatchEvent { steam_game: true, ..e.clone() };
        assert!(classify_event(&steam, &cfg).is_none(), "{name} in a Steam library");
    }
    assert!(classify_event(&ev("scrgame", "Scr Game", 9, 0x9, true), &cfg).is_some(), "only the .scr extension");
}

#[test]
fn store_media_apps_are_denied() {
    // R2-5: with G6 a UWP frame resolves to the hosted exe, no longer the denied
    // ApplicationFrameHost; without trace rights a fullscreen one would count as a game.
    let cfg = Config::default();
    for stem in [
        "Video.UI",
        "Microsoft.Media.Player",
        "Microsoft.Photos",
        "WinStore.App",
        "SystemSettings",
        "Netflix",
        "WWAHost",
        // R3-detect-2 (exe names read from the AppxManifests).
        "WindowsCamera",
        "HxCalendarAppImm",
        "XboxPcAppCE",
    ] {
        assert!(!is_target(stem, true, &cfg), "{stem}");
    }
    assert!(is_target("Minecraft.Windows", true, &cfg), "a Store game still is");
}
