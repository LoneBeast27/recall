//! App-track parsing, names and the silence clock (no WASAPI, no processes).

use std::time::Duration;

use super::{parse, AppSource, SilenceClock};

#[test]
fn the_list_parses_and_bad_entries_are_refused() {
    let apps = parse(r#"[{"exe":"player.exe","label":"Source A"},{"exe":"chat.exe"}]"#).unwrap();
    assert_eq!(apps.len(), 2);
    assert_eq!(apps[1].label, "");
    assert!(parse(r#"[{"exe":"C:\\apps\\a.exe"}]"#).is_err(), "no folders");
    assert!(parse(r#"[{"exe":""}]"#).is_err());
    let seven: Vec<String> = (0..7).map(|n| format!(r#"{{"exe":"a{n}.exe"}}"#)).collect();
    assert!(parse(&format!("[{}]", seven.join(","))).is_err(), "at most 6");
    assert!(parse("not json").is_err());
}

#[test]
fn a_track_is_named_by_its_label_else_the_exe_without_its_extension() {
    let app = |exe: &str, label: &str| AppSource { exe: exe.into(), label: label.into() };
    assert_eq!(app("player.exe", " Source A ").track_name(), "Source A");
    assert_eq!(app("player.EXE", "").track_name(), "player");
    assert_eq!(app("tool", "").track_name(), "tool");
}

#[test]
fn silence_is_owed_only_past_the_slack_and_keeps_the_track_level_with_the_clock() {
    let mut clock = SilenceClock::new(48_000);
    let slack = Duration::from_millis(100);
    assert_eq!(clock.owed(Duration::from_millis(50), slack), 0, "within the slack: wait for sound");
    assert_eq!(clock.owed(Duration::from_millis(200), slack), 9_600, "200 ms behind: all of it");
    clock.wrote(4_800);
    assert_eq!(clock.owed(Duration::from_millis(300), slack), 0, "the sound kept up");
    assert_eq!(clock.owed(Duration::from_millis(1_000), Duration::ZERO), 33_600, "closed app: silence to the clock");
}
