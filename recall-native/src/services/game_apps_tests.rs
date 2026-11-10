use super::*;
use serde_json::json;

/// The controller's config as its `config` event sends it (made-up apps).
fn config() -> Value {
    json!({
        "behavior": { "default_game_class": "sdr_game" },
        "overrides": [
            { "exe": "hdr-testcard", "kind": "hdr_game" },
            { "exe": "OldTool.exe", "kind": "none" },
            { "exe": "fixturegame", "kind": "sdr_game", "clamp": "accurate" }
        ],
        "detect": { "fullscreen_games": ["Discord"] }
    })
}

fn app(exe: &str) -> PlayingApp {
    PlayingApp { exe: exe.into(), label: exe.into(), path: format!("X:\\Apps\\{exe}") }
}

#[test]
fn the_list_shows_every_registration() {
    let list = entries(Some(&config()));
    assert_eq!(
        list,
        vec![
            GameEntry { exe: "hdr-testcard".into(), how: Registration::Record },
            GameEntry { exe: "OldTool.exe".into(), how: Registration::Never },
            GameEntry { exe: "fixturegame".into(), how: Registration::Record },
            GameEntry { exe: "Discord".into(), how: Registration::Fullscreen },
        ]
    );
    assert!(entries(None).is_empty(), "no controller: nothing to show");
    assert!(entries(Some(&json!({}))).is_empty());
}

#[test]
fn an_exe_is_matched_with_or_without_exe_g9() {
    assert!(same_exe("Game.exe", "game") && same_exe("GAME", "game.EXE") && !same_exe("game", "gamer"));
    assert!(!same_exe("", ""), "an empty name matches nothing");
}

#[test]
fn add_app_offers_running_apps_once_each_minus_recall_and_the_shell() {
    let found = vec![app("recall-native.exe"), app("FixtureGame.exe"), app("explorer.exe"), app("fixturegame.EXE"), app("Tool.exe"), app("ApplicationFrameHost.exe")];
    let found = found.into_iter().enumerate().map(|(i, a)| (i as u32 + 1, a)).collect();
    let offered: Vec<String> = pickable(found, None).apps.into_iter().map(|a| a.exe).collect();
    assert_eq!(offered, ["FixtureGame.exe", "Tool.exe"], "z-order kept");
}

#[test]
fn in_front_is_the_foreground_window_not_the_first_found_r8_detect_3() {
    // A topmost overlay comes first in z-order; the game was the foreground window.
    let found = vec![(7, app("Overlay.exe")), (8, app("Chatty.exe")), (9, app("PopupGame.exe"))];
    let pick = pickable(found.clone(), Some(9));
    assert!(pick.in_front);
    assert_eq!(pick.apps.iter().map(|a| a.exe.as_str()).collect::<Vec<_>>(), ["PopupGame.exe", "Overlay.exe", "Chatty.exe"]);
    let recall_in_front = pickable(found, Some(4242));
    assert!(!recall_in_front.in_front, "Recall itself in front: no app is labelled");
    assert_eq!(recall_in_front.apps[0].exe, "Overlay.exe");
}

#[test]
fn borderless_popup_game_windows_are_offered_r8_detect_3() {
    // WS_POPUP with no caption, no title and no APPWINDOW: SDL's borderless window.
    let popup = WinInfo { visible: true, owned: false, exstyle: 0, size: (1920, 1080) };
    assert!(is_app_window(&popup), "SDL / popup fullscreen window, no caption, no APPWINDOW, any title");
    assert!(!is_app_window(&WinInfo { visible: false, ..popup }));
    assert!(!is_app_window(&WinInfo { owned: true, ..popup }), "a dialog of another window");
    assert!(!is_app_window(&WinInfo { exstyle: 0x80, ..popup }), "a tool window");
    assert!(is_app_window(&WinInfo { exstyle: 0x80 | 0x4_0000, ..popup }), "a tool window marked APPWINDOW is on the taskbar");
    assert!(!is_app_window(&WinInfo { size: (0, 0), ..popup }), "zero size");
}

#[test]
fn clicking_the_selected_segment_sends_nothing_r8_detect_4() {
    let recorded = GameEntry { exe: "hdr-testcard".into(), how: Registration::Record };
    assert_eq!(segment_command(&recorded, Registration::Record), None, "no rewrite of hdr_game / its clamp");
    assert_eq!(segment_command(&recorded, Registration::Never), Some(DaemonCommand::RegisterGame { exe: "hdr-testcard".into(), how: "never" }));
    let never = GameEntry { exe: "OldTool.exe".into(), how: Registration::Never };
    assert_eq!(segment_command(&never, Registration::Never), None);
}

#[test]
fn every_games_edit_is_one_app_not_a_whole_list_r8_detect_7() {
    // Built from no snapshot: the controller applies each to its current config.
    assert_eq!(add_command("NewGame.exe"), DaemonCommand::RegisterGame { exe: "NewGame.exe".into(), how: "record" });
    let entry = GameEntry { exe: "OldTool.exe".into(), how: Registration::Never };
    assert_eq!(remove_command(&entry), DaemonCommand::RegisterGame { exe: "OldTool.exe".into(), how: "remove" });
    let discord = GameEntry { exe: "Discord".into(), how: Registration::Fullscreen };
    assert_eq!(remove_command(&discord), DaemonCommand::RegisterGame { exe: "Discord".into(), how: "unlist" });
}

/// R2-detect-r10-4: "Add app…" never offers what the controller refuses.
#[test]
fn add_app_never_offers_a_part_of_windows() {
    let found = ["fontdrvhost.exe", "smartscreen.exe", "consent.exe", "SystemSettings.exe", "dwm.exe", "Game.exe"].iter().enumerate().map(|(i, e)| (i as u32 + 1, app(e))).collect();
    let offered: Vec<String> = pickable(found, None).apps.into_iter().map(|a| a.exe).collect();
    assert_eq!(offered, vec!["Game.exe".to_string()]);
}

/// Gate p5-3: a screensaver or a hand-edited Windows entry never shows as "Recorded", and a
/// running screensaver is not offered.
#[test]
fn windows_entries_and_screensavers_never_show_as_recorded() {
    let config = json!({
        "overrides": [
            { "exe": "explorer", "kind": "sdr_game" },
            { "exe": r"C:\Windows\Explorer.EXE", "kind": "hdr_game" },
            { "exe": "Mystify.scr", "kind": "sdr_game" }
        ],
        "detect": { "fullscreen_games": ["Mystify.scr"] }
    });
    for entry in entries(Some(&config)) {
        assert!(!matches!(entry.how, Registration::Record | Registration::Fullscreen), "{entry:?}");
    }
    let offered: Vec<String> = pickable(vec![(1, app("Mystify.scr")), (2, app("Game.exe"))], None).apps.into_iter().map(|a| a.exe).collect();
    assert_eq!(offered, vec!["Game.exe".to_string()]);
}
