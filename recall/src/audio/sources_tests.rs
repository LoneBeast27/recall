use super::*;

fn env(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
    let map: std::collections::HashMap<String, String> = pairs.iter().map(|(k, v)| ((*k).to_owned(), (*v).to_owned())).collect();
    move |key| map.get(key).cloned()
}

fn rows(list: &[SourceSpec]) -> Vec<(&str, &str, &str, &str, &str)> {
    list.iter().map(|s| (s.id.as_str(), s.kind.as_str(), s.name.as_str(), s.device.as_str(), s.exe.as_str())).collect()
}

#[test]
fn the_list_builds_tracks_in_order_with_names_devices_and_gain() {
    let json = r#"[
        {"id":"game","kind":"game","name":"Game"},
        {"id":"in-1","kind":"input","device":"Microphone (USB Audio Device)","name":"Voice","gain_db":-6},
        {"id":"out-1","kind":"output","device":"","name":"Desktop"},
        {"id":"app-1","kind":"app","exe":"player.exe","name":"Music","gain_db":12}
    ]"#;
    let list = resolve(env(&[(SOURCES_ENV, json), ("RECALL_AUDIO_MIC_OFF", "1")])).unwrap();
    assert_eq!(
        rows(&list),
        [
            ("game", "game", "Game", "", ""),
            ("in-1", "input", "Voice", "Microphone (USB Audio Device)", ""),
            ("out-1", "output", "Desktop", "", ""),
            ("app-1", "app", "Music", "", "player.exe"),
        ],
        "the list wins over the old variables"
    );
    assert!((list[1].gain() - 0.501).abs() < 0.001, "-6 dB");
    assert!((list[3].gain() - 3.981).abs() < 0.001, "+12 dB");
    assert_eq!(list[0].gain(), 1.0);
    let apps = app_sources(&list);
    assert_eq!((apps.len(), apps[0].id.as_str(), apps[0].label.as_str(), apps[0].gain_db), (1, "app-1", "Music", 12.0));
    assert!(resolve(env(&[(SOURCES_ENV, "[]")])).unwrap().is_empty(), "an empty list records no audio");
}

#[test]
fn bad_entries_are_skipped_and_unreadable_lists_refused() {
    for json in [
        r#"[{"id":"a","kind":"bus","name":"A"}]"#,
        r#"[{"id":"a","kind":"app","name":"A"}]"#,
        r#"[{"id":"a","kind":"app","exe":"C:\\apps\\a.exe","name":"A"}]"#,
        r#"[{"id":" ","kind":"input","name":"A"}]"#,
        r#"[{"id":"a","kind":"input","name":"A","gain_db":"loud"}]"#,
    ] {
        assert_eq!(resolve(env(&[(SOURCES_ENV, json)])), Ok(Vec::new()), "{json}");
    }
    assert!(resolve(env(&[(SOURCES_ENV, "not json")])).is_err(), "unreadable JSON");
}

/// The reason each skipped entry gives (logged by `parse`).
#[test]
fn skipped_entries_say_why() {
    let json = r#"[{"id":"mic","kind":"input","name":"Mic"},{"id":"MIC","kind":"input","name":"Copy"},{"id":"x","kind":"Input","name":"Typo"}]"#;
    let (list, skipped) = parse_entries(json).unwrap();
    assert_eq!(list.len(), 1);
    assert!(skipped[0].contains("'MIC'") && skipped[0].contains("used twice"), "{skipped:?}");
    assert!(skipped[1].contains("unknown kind 'Input'"), "{skipped:?}");
    let nine: Vec<String> = (1..=9).map(|n| format!(r#"{{"id":"o{n}","kind":"output","name":"{n}"}}"#)).collect();
    let (list, skipped) = parse_entries(&format!("[{}]", nine.join(","))).unwrap();
    assert_eq!(list.len(), 8);
    assert!(skipped[0].contains("'o9'") && skipped[0].contains("at most 8"), "{skipped:?}");
}

#[test]
fn without_the_list_the_old_variables_still_build_the_same_tracks() {
    let list = resolve(env(&[])).unwrap();
    assert_eq!(rows(&list), [("game", "game", "Game", "", ""), ("mic", "input", "Microphone", "", "")], "Windows defaults");
    let list = resolve(env(&[
        ("RECALL_AUDIO_GAME", "Speakers (USB Audio Device)"),
        ("RECALL_AUDIO_MIC", "Microphone (USB Audio Device)"),
        ("RECALL_AUDIO_VOICE_EXE", "chat.exe"),
        (apps::APPS_ENV, r#"[{"exe":"player.exe","label":"Music"}]"#),
    ]))
    .unwrap();
    assert_eq!(
        rows(&list),
        [
            ("game", "game", "Game", "Speakers (USB Audio Device)", ""),
            ("mic", "input", "Microphone", "Microphone (USB Audio Device)", ""),
            ("voice", "app", "Voice chat", "", "chat.exe"),
            ("app-1", "app", "Music", "", "player.exe"),
        ]
    );
    let no_mic = resolve(env(&[("RECALL_AUDIO_MIC_OFF", "1")])).unwrap();
    assert_eq!(rows(&no_mic), [("game", "game", "Game", "", "")]);
}

#[test]
fn rings_follow_the_list_order_and_carry_title_and_tag() {
    let list = resolve(env(&[(SOURCES_ENV, r#"[{"id":"app-1","kind":"app","exe":"a.exe","name":"A"},{"id":"in","kind":"input","name":"Voice"}]"#)])).unwrap();
    let ring = |spec: &SourceSpec, tag| Arc::new(TrackRing::for_source(spec, tag, 1, String::new()));
    let ordered = in_list_order(&list, vec![ring(&list[1], "mic"), ring(&list[0], "app:a.exe")]);
    let got: Vec<(&str, &str, &str)> = ordered.iter().map(|r| (r.name, r.title(), r.source)).collect();
    assert_eq!(got, [("app-1", "A", "app:a.exe"), ("in", "Voice", "mic")]);
}

/// 2026-11-07 audit A6 (engine side): one bad entry in the list (a duplicate id from a
/// copy-pasted block, a typo in a kind, a ninth source) is skipped; the other sources, the game
/// included, still record. The whole list used to be refused: clips had no audio at all.
#[test]
fn one_bad_entry_skips_only_itself() {
    let json = r#"[
        {"id":"game","kind":"game","name":"Game"},
        {"id":"mic","kind":"input","name":"Mic"},
        {"id":"MIC","kind":"input","name":"Copy"},
        {"id":"x","kind":"Input","name":"Typo"},
        {"id":"o1","kind":"output","name":"1"},{"id":"o2","kind":"output","name":"2"},{"id":"o3","kind":"output","name":"3"},
        {"id":"o4","kind":"output","name":"4"},{"id":"o5","kind":"output","name":"5"},{"id":"o6","kind":"output","name":"6"},
        {"id":"o7","kind":"output","name":"7"}
    ]"#;
    let ids: Vec<String> = parse(json).expect("the good entries").iter().map(|s| s.id.clone()).collect();
    assert_eq!(ids, ["game", "mic", "o1", "o2", "o3", "o4", "o5", "o6"], "the first good eight");
}
