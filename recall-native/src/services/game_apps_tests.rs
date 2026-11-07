use super::*;

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
fn adding_an_app_writes_a_game_override_and_keeps_the_others() {
    let patch = register_patch(&config(), "NewGame.exe", Registration::Record);
    let list = patch["overrides"].as_array().expect("the whole overrides array");
    assert_eq!(list.len(), 4);
    assert_eq!(list[3], json!({ "exe": "NewGame.exe", "kind": "sdr_game" }));
    assert_eq!(list[2], json!({ "exe": "fixturegame", "kind": "sdr_game", "clamp": "accurate" }), "clamp kept");
    let mut hdr = config();
    hdr["behavior"]["default_game_class"] = json!("hdr_game");
    assert_eq!(register_patch(&hdr, "NewGame.exe", Registration::Record)["overrides"][3]["kind"], "hdr_game");
}

#[test]
fn an_override_is_matched_with_or_without_exe_g9() {
    // "FixtureGame.exe" replaces the "fixturegame" entry rather than adding a second one.
    let patch = register_patch(&config(), "FixtureGame.exe", Registration::Never);
    let list = patch["overrides"].as_array().unwrap();
    assert_eq!(list.len(), 3);
    assert_eq!(list[2], json!({ "exe": "FixtureGame.exe", "kind": "none" }));
    assert!(same_exe("Game.exe", "game") && same_exe("GAME", "game.EXE") && !same_exe("game", "gamer"));
    assert!(!same_exe("", ""), "an empty name matches nothing");
}

#[test]
fn never_record_and_remove() {
    let never = register_patch(&config(), "Chatty.exe", Registration::Never);
    assert_eq!(never["overrides"][3], json!({ "exe": "Chatty.exe", "kind": "none" }));
    let entry = GameEntry { exe: "OldTool.exe".into(), how: Registration::Never };
    let removed = remove_patch(&config(), &entry);
    let left: Vec<&str> = removed["overrides"].as_array().unwrap().iter().map(|o| o["exe"].as_str().unwrap()).collect();
    assert_eq!(left, ["hdr-testcard", "fixturegame"]);
    let discord = GameEntry { exe: "discord.exe".into(), how: Registration::Fullscreen };
    assert_eq!(remove_patch(&config(), &discord), json!({ "detect": { "fullscreen_games": [] } }));
    assert_eq!(register_patch(&config(), "Discord.exe", Registration::Fullscreen), json!({ "detect": { "fullscreen_games": ["Discord"] } }), "no duplicate");
}

#[test]
fn add_app_offers_running_apps_once_each_minus_recall_and_the_shell() {
    let found = vec![app("recall-native.exe"), app("FixtureGame.exe"), app("explorer.exe"), app("fixturegame.EXE"), app("Tool.exe"), app("ApplicationFrameHost.exe")];
    let offered: Vec<String> = pickable(found).into_iter().map(|a| a.exe).collect();
    assert_eq!(offered, ["FixtureGame.exe", "Tool.exe"], "front-most first");
}
