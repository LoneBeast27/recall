use super::*;

fn list() -> Vec<Source> {
    from_config(Some(&json!([
        {"id": "game", "kind": "game", "name": "Game"},
        {"id": "mic", "kind": "input", "device": "Microphone (USB Audio Device)", "name": "Microphone", "enabled": false, "gain_db": -3.5}
    ])))
}

#[test]
fn the_saved_list_round_trips_and_bad_entries_are_skipped() {
    let sources = list();
    assert_eq!(sources.len(), 2);
    assert_eq!((sources[1].kind, sources[1].enabled, sources[1].gain_db), (Kind::Input, false, -3.5));
    assert_eq!(from_config(Some(&to_config(&sources))), sources);
    let odd = from_config(Some(&json!([{"id": "x", "kind": "bus", "name": "X"}, {"kind": "input", "name": "No id"}, {"id": "ok", "kind": "app", "exe": "player.exe", "name": "Music"}])));
    assert_eq!(odd.iter().map(|s| s.id.as_str()).collect::<Vec<_>>(), ["ok"]);
    assert!(from_config(None).is_empty());
}

#[test]
fn new_sources_get_unused_ids_and_names_and_one_game_at_most() {
    let sources = list();
    assert_eq!(addable(&sources), [Kind::Input, Kind::Output, Kind::App], "a game is already there");
    let more = with_new(&sources, Kind::Input, "", "").unwrap();
    let added = more.last().unwrap();
    assert_eq!((added.id.as_str(), added.name.as_str(), added.device.as_str(), added.enabled), ("input-1", "Microphone 2", "", true));
    let app = with_new(&more, Kind::App, " player.exe ", "Media Player\n").unwrap();
    assert_eq!((app[3].exe.as_str(), app[3].name.as_str(), app[3].id.as_str()), ("player.exe", "Media Player", "app-1"));
    assert!(with_new(&sources, Kind::App, " ", "x").is_none(), "an app source needs an exe");
    assert!(with_new(&sources, Kind::Game, "", "").is_none());
    let full: Vec<Source> = (0..SOURCES_MAX).map(|n| Source { id: format!("s{n}"), ..sources[1].clone() }).collect();
    assert!(addable(&full).is_empty() && with_new(&full, Kind::Output, "", "").is_none());
    assert_eq!(addable(&[]), [Kind::Input, Kind::Output, Kind::App, Kind::Game], "removed game can come back");
}

#[test]
fn the_fader_spans_minus_60_to_plus_12_db_in_half_steps() {
    assert_eq!(gain_at(0.0), -60.0);
    assert_eq!(gain_at(1.0), 12.0);
    assert_eq!(gain_at(fader_fraction(0.0)), 0.0);
    assert_eq!(gain_at(0.5), -24.0);
    assert_eq!(gain_at(0.8341), 0.0, "rounded to 0.5 dB");
    assert_eq!(fader_fraction(40.0), 1.0);
    assert_eq!((gain_label(0.02), gain_label(3.0), gain_label(-3.5), gain_label(-60.0)), ("0 dB".into(), "+3 dB".into(), "-3.5 dB".into(), "-60 dB".into()));
    assert_eq!(clip_name(&"x".repeat(80)).chars().count(), NAME_MAX);
}

#[test]
fn a_dragged_source_moves_to_its_slot_and_keeps_the_rest_in_order() {
    let three = from_config(Some(&json!([
        {"id": "a", "kind": "game", "name": "A"},
        {"id": "b", "kind": "input", "name": "B"},
        {"id": "c", "kind": "output", "name": "C"}
    ])));
    let ids = |l: &[Source]| l.iter().map(|s| s.id.clone()).collect::<Vec<_>>();
    assert_eq!(ids(&moved(&three, "b", 0)), ["b", "a", "c"]);
    assert_eq!(ids(&moved(&three, "a", 2)), ["b", "c", "a"]);
    assert_eq!(ids(&moved(&three, "a", 9)), ["b", "c", "a"], "past the end goes last");
    assert_eq!(moved(&three, "c", 2), three, "same slot, same list");
    assert_eq!(moved(&three, "gone", 0), three, "an unknown id changes nothing");
    let saved = moved(&three, "c", 0);
    assert_eq!(ids(&from_config(Some(&to_config(&saved)))), ["c", "a", "b"], "the saved order is the new track order");
}

#[test]
fn the_drop_slot_follows_the_pointer_and_stays_in_range() {
    let (left, pitch) = (100.0, 168.0);
    assert_eq!(drop_slot(100.0 + 78.0, left, pitch, 3), 0, "the middle of the first strip");
    assert_eq!(drop_slot(100.0 + 168.0 + 5.0, left, pitch, 3), 1);
    assert_eq!(drop_slot(100.0 + 2.0 * 168.0 + 80.0, left, pitch, 3), 2);
    assert_eq!(drop_slot(-500.0, left, pitch, 3), 0, "left of the row is the first slot");
    assert_eq!(drop_slot(5000.0, left, pitch, 3), 2, "right of the row is the last slot");
    assert_eq!(drop_slot(300.0, left, pitch, 0), 0);
}

#[test]
fn escape_cancels_only_a_live_strip_drag() {
    assert!(escape_cancels_drag("escape", false, true));
    assert!(!escape_cancels_drag("escape", false, false), "no drag: Escape goes on (closes Settings)");
    assert!(!escape_cancels_drag("escape", true, true), "Shift+Escape is not a cancel");
    assert!(!escape_cancels_drag("enter", false, true));
}

#[test]
fn a_let_go_fader_keeps_its_gain_until_the_save_comes_back() {
    use std::time::Duration;
    let ms = Duration::from_millis;
    assert_eq!(shown_gain(Some(-6.0), Some((3.0, ms(10))), 0.0), -6.0, "dragging wins");
    assert_eq!(shown_gain(None, Some((-6.0, ms(40))), 0.0), -6.0, "saved list still old: no snap back");
    assert_eq!(shown_gain(None, Some((-6.0, ms(60))), -6.0), -6.0, "save came back");
    assert_eq!(shown_gain(None, Some((-6.0, PENDING_GAIN_FOR)), 0.0), 0.0, "a save that never lands gives way");
    assert_eq!(shown_gain(None, None, 4.5), 4.5);
}

/// 2026-11-07 audit A11: Undo after a remove and an add of the same kind (the add took the
/// freed id) restores the source under a fresh id instead of silently doing nothing.
#[test]
fn undo_restores_a_source_whose_id_was_reused() {
    let sources = list();
    let mut after = sources.clone();
    let removed = after.remove(1);
    let reused = with_new(&after, Kind::Input, "", "").unwrap();
    let mut reused = reused;
    reused[1].id = removed.id.clone(); // the add took the freed id
    let back = restored(&reused, 1, removed.clone()).expect("Undo still restores it");
    assert_eq!(back.len(), 3);
    assert_eq!((back[1].name.as_str(), back[1].device.as_str()), ("Microphone", "Microphone (USB Audio Device)"), "the removed source, in its place");
    assert_ne!(back[1].id.to_lowercase(), back[2].id.to_lowercase(), "ids stay unique");
    let full: Vec<Source> = (0..SOURCES_MAX).map(|n| Source { id: format!("s{n}"), ..sources[1].clone() }).collect();
    assert!(restored(&full, 0, removed).is_none(), "a full list cannot take it back");
    let game = sources[0].clone();
    let other_game = vec![Source { id: "game-1".into(), ..game.clone() }];
    assert!(restored(&other_game, 0, game).is_none(), "never a second game");
}

/// 2026-11-07 audit A12: an input says whether the mic-mute hotkey silences it; a list saved
/// before the flag says nothing, so the endpoint's form factor decides.
#[test]
fn an_input_keeps_its_mic_mute_flag() {
    let sources = from_config(Some(&json!([{"id": "in-1", "kind": "input", "name": "Console", "mic_mute": false}, {"id": "mic", "kind": "input", "name": "Mic"}])));
    assert_eq!((sources[0].mic_mute, sources[1].mic_mute), (Some(false), None));
    assert_eq!(from_config(Some(&to_config(&sources))), sources, "round trip");
    assert!(to_config(&sources)[1].get("mic_mute").is_none(), "no flag is saved as none");
}

/// 2026-11-07 audit R2-deep-audio-3: Alt+M mutes only microphones (SPEC A12). "+ Add source"
/// offers "Microphone" and "Line-in or capture card", each saying so; an input without a flag
/// (an old config, the default mic) follows its endpoint's form factor, never its name.
#[test]
fn a_line_in_is_not_muted_by_the_mic_hotkey() {
    use crate::services::audio_devices::DeviceLists;
    let base = from_config(Some(&json!([{"id": "game", "kind": "game", "name": "Game"}])));
    let line = with_new_input(&base, false).unwrap();
    let added = line.last().unwrap();
    assert_eq!((added.kind, added.mic_mute, added.name.as_str()), (Kind::Input, Some(false), "Line-in"));
    assert_eq!(with_new_input(&base, true).unwrap().last().unwrap().mic_mute, Some(true));
    // The form factor (injected: the device lists say which endpoints are microphones).
    let lists = DeviceLists {
        inputs: vec!["Capture In (Box One)".into(), "Mic (Box Two)".into()],
        mic_inputs: vec!["Mic (Box Two)".into()],
        default_input_is_mic: Some(false),
        ..DeviceLists::default()
    };
    let input = |device: &str, flag: Option<bool>| Source { id: "in-1".into(), kind: Kind::Input, device: device.into(), exe: String::new(), name: "In".into(), enabled: true, gain_db: 0.0, mic_mute: flag };
    assert!(!mutes(&input("Capture In (Box One)", None), Some(&lists)), "a line-level endpoint keeps its sound");
    assert!(mutes(&input("Mic (Box Two)", None), Some(&lists)), "a microphone endpoint mutes");
    assert!(!mutes(&input("", None), Some(&lists)), "the Windows default, by its own form factor");
    assert!(mutes(&input("Capture In (Box One)", Some(true)), Some(&lists)), "the strip's toggle wins");
    assert!(!mutes(&input("Mic (Box Two)", Some(false)), Some(&lists)));
    assert!(mutes(&input("Capture In (Box One)", None), None), "lists not read yet: shown as muted");
}

/// 2026-11-07 audit R2-deep-audio-5: a repeated id (case-insensitive) is shown once, as the
/// engine records it once; the first entry wins, like the engine.
#[test]
fn a_duplicate_source_id_is_shown_once() {
    let sources = from_config(Some(&json!([{"id": "mic", "kind": "input", "name": "First"}, {"id": " MIC ", "kind": "input", "name": "Copy"}, {"id": "game", "kind": "game", "name": "Game"}])));
    let names: Vec<&str> = sources.iter().map(|s| s.name.as_str()).collect();
    assert_eq!(names, ["First", "Game"]);
}
