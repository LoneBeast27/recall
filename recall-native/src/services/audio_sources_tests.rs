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
