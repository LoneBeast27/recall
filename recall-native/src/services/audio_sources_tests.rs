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
    assert_eq!((added.id.as_str(), added.name.as_str(), added.device.as_str(), added.enabled), ("input-1", "Input", "", true));
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

/// 2026-11-07 audit R3-deep-audio-1: an input without a flag (a config written before it, the
/// default mic) is muted by Alt+M whatever Windows calls its endpoint, as before the flag. Only
/// A custom input follows Alt+M by default. Its saved device and mute flag control line-in behavior.
#[test]
fn an_old_input_is_muted_by_the_mic_hotkey_whatever_its_form_factor() {
    use crate::services::audio_devices::DeviceLists;
    // The form factor (injected): "Line (Box One)" is line-level, so it is not a microphone.
    let lists = DeviceLists {
        inputs: vec!["Line (Box One)".into(), "Mic (Box Two)".into()],
        line_inputs: vec!["Line (Box One)".into()],
        default_input_is_line: Some(true),
        ..DeviceLists::default()
    };
    let old = from_config(Some(&json!([{"id": "mic", "kind": "input", "name": "Mic", "device": "Line (Box One)"}, {"id": "in-2", "kind": "input", "name": "Default"}])));
    assert_eq!((old[0].mic_mute, old[1].mic_mute), (None, None));
    assert!(mutes(&old[0], None), "an old line-level input is muted");
    assert!(mutes(&old[1], None), "an old input on a line-level Windows default is muted");
    // The form factor is only a hint in the strip menu.
    assert!(windows_says_line_level(&old[0], Some(&lists)) && windows_says_line_level(&old[1], Some(&lists)));
    let mic_endpoint = Source { device: "Mic (Box Two)".into(), ..old[0].clone() };
    assert!(!windows_says_line_level(&mic_endpoint, Some(&lists)), "a microphone endpoint: no hint");
    assert!(!windows_says_line_level(&old[0], None), "lists not read yet: no hint");
    let base = from_config(Some(&json!([{"id": "game", "kind": "game", "name": "Game"}])));
    let input = with_new(&base, Kind::Input, "", "").unwrap();
    assert_eq!(input.last().unwrap().name, "Input");
    let added = &Source { mic_mute: Some(false), ..input.last().unwrap().clone() };
    assert!(mutes(added, None), "a new line-in on the Windows default follows the mic hotkey (R4-deep-audio-1)");
    assert!(!mutes(&Source { device: "Line (Box One)".into(), device_id: "{one}".into(), ..added.clone() }, None), "a named line-in with its id keeps its sound");
    assert!(mutes(&Source { device: "Line (Box One)".into(), ..added.clone() }, None), "no id saved: follows the mic hotkey (R6-deep-audio-1)");
    let mic = with_new(&base, Kind::Input, "", "").unwrap();
    assert!(mutes(mic.last().unwrap(), None), "a new microphone is muted");
}

/// 2026-11-07 audit R4-deep-audio-1: a line-in or capture card skips Alt+M only on a named device.
/// On the Windows default (a new "Line-in", or one set back to it) it records the default input,
/// normally the user's microphone, so it follows the mic hotkey and the strip says so.
#[test]
fn a_line_in_follows_the_mic_hotkey_until_it_names_a_device() {
    let base = from_config(Some(&json!([{"id": "game", "kind": "game", "name": "Game"}])));
    let added = Source { mic_mute: Some(false), ..with_new(&base, Kind::Input, "", "").unwrap().pop().unwrap() };
    assert_eq!((added.mic_mute, added.device.as_str()), (Some(false), ""));
    assert!(mutes(&added, None), "a new line-in on the Windows default is muted by the mic hotkey");
    assert!(line_in_on_default(&added), "the strip notes it");
    let named = Source { device: "Line (Box One)".into(), device_id: "{one}".into(), ..added.clone() };
    assert!(!mutes(&named, None) && !line_in_on_default(&named), "a named line-in with its id keeps its sound");
    let reset = Source { device: " ".into(), ..named.clone() };
    assert!(mutes(&reset, None) && line_in_on_default(&reset), "set back to the Windows default: follows Alt+M again");
    let mic = Source { mic_mute: Some(true), ..added.clone() };
    assert!(mutes(&mic, None) && !line_in_on_default(&mic), "a microphone: no note");
    let output = Source { kind: Kind::Output, mic_mute: None, ..added.clone() };
    assert!(!mutes(&output, None), "only inputs mute");
}

/// The strip's warning line: a line-in on the Windows default says it follows the mic hotkey;
/// a missing device and twin names keep their own words (counted: R5-deep-audio-4).
#[test]
fn the_strip_notes_a_line_in_on_the_windows_default() {
    use crate::services::audio_devices::DeviceLists;
    let base = from_config(Some(&json!([{"id": "game", "kind": "game", "name": "Game"}])));
    let added = Source { mic_mute: Some(false), ..with_new(&base, Kind::Input, "", "").unwrap().pop().unwrap() };
    assert_eq!(strip_note(&added, None).as_deref(), Some(LINE_IN_ON_DEFAULT));
    let named = Source { device: "Line (Box One)".into(), device_id: "{one}".into(), ..added.clone() };
    let at = |inputs: &[&str]| DeviceLists { inputs: inputs.iter().map(|n| (*n).to_owned()).collect(), input_ids: vec!["{one}".into(); inputs.len()], ..DeviceLists::default() };
    assert_eq!(strip_note(&named, Some(&at(&["Line (Box One)"]))), None);
    let no_id = Source { device_id: String::new(), ..named.clone() };
    assert_eq!(strip_note(&no_id, Some(&at(&["Line (Box One)"]))).as_deref(), Some(crate::services::audio_mute::NO_ID), "no id saved (R6-deep-audio-1)");
    assert_eq!(strip_note(&named, Some(&at(&["Mic (Box Two)"]))).as_deref(), Some("Not connected"));
    assert_eq!(strip_note(&named, Some(&at(&["Line (Box One)"; 3]))).as_deref(), Some("3 devices share this name"));
    let renumbered = Source { device: "3- Line (Box One)".into(), ..named.clone() };
    assert_eq!(strip_note(&renumbered, Some(&at(&["4- Line (Box One)", "5- Line (Box One)"]))).as_deref(), Some("2 devices match this name"), "both connected: ambiguous, not missing");
    let mic = with_new(&base, Kind::Input, "", "").unwrap().pop().unwrap();
    assert_eq!(strip_note(&mic, None), None, "a microphone on the default: no note");
}

/// 2026-11-07 audit R4-deep-audio-2: the "line-level" hint shows only for an endpoint Windows
/// calls line-level, not for an unknown (many virtual mics), S/PDIF or HDMI one.
#[test]
fn the_line_level_hint_names_only_line_level_endpoints() {
    use crate::services::audio_devices::DeviceLists;
    let lists = DeviceLists {
        inputs: vec!["Line (Box One)".into(), "Mic (Box Two)".into(), "Virtual (Box Three)".into()],
        line_inputs: vec!["Line (Box One)".into()],
        default_input_is_line: Some(false),
        ..DeviceLists::default()
    };
    let at = |device: &str| Source { communications: false, icon: String::new(), id: "in-1".into(), kind: Kind::Input, device: device.into(), device_id: String::new(), exe: String::new(), name: "In".into(), enabled: true, gain_db: 0.0, mic_mute: None };
    assert!(windows_says_line_level(&at("Line (Box One)"), Some(&lists)), "line-level: hint");
    assert!(!windows_says_line_level(&at("Mic (Box Two)"), Some(&lists)), "a microphone: no hint");
    assert!(!windows_says_line_level(&at("Virtual (Box Three)"), Some(&lists)), "an unknown form factor is not called line-level");
    assert!(!windows_says_line_level(&at(""), Some(&lists)), "a default that is not line-level: no hint");
    let line_default = DeviceLists { default_input_is_line: Some(true), ..lists.clone() };
    assert!(windows_says_line_level(&at(""), Some(&line_default)), "a line-level default: hint");
    assert!(!windows_says_line_level(&at("Line (Box One)"), None), "lists not read yet: no hint");
}

/// 2026-11-07 audit R2-deep-audio-5: a repeated id (case-insensitive) is shown once, as the
/// engine records it once; the first entry wins, like the engine.
#[test]
fn a_duplicate_source_id_is_shown_once() {
    let sources = from_config(Some(&json!([{"id": "mic", "kind": "input", "name": "First"}, {"id": " MIC ", "kind": "input", "name": "Copy"}, {"id": "game", "kind": "game", "name": "Game"}])));
    let names: Vec<&str> = sources.iter().map(|s| s.name.as_str()).collect();
    assert_eq!(names, ["First", "Game"]);
}
