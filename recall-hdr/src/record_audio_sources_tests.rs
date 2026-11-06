use super::*;
use crate::record_app_audio::AppAudioSource;

fn json_lines(lines: &[String]) -> Vec<serde_json::Value> {
    lines.iter().map(|l| serde_json::from_str(l).unwrap()).collect()
}

fn ids(list: &[AudioSource]) -> Vec<&str> {
    list.iter().map(|s| s.id.as_str()).collect()
}

#[test]
fn a_fresh_install_records_the_game_and_the_default_microphone() {
    let list = default_sources();
    assert_eq!(ids(&list), ["game", "mic"]);
    assert_eq!((list[0].kind.as_str(), list[1].kind.as_str()), ("game", "input"));
    assert!(list.iter().all(|s| s.device.is_empty() && s.enabled && s.gain_db == 0.0), "Windows defaults, no names");
    assert_eq!(RecordingCfg::default().audio_sources, list);
    assert!(validate(&list).is_ok());
}

#[test]
fn old_keys_become_sources_without_writing_any_new_names() {
    let rec = RecordingCfg {
        desktop_audio_device: "Speakers (USB Audio Device)".into(),
        mic_device: "Microphone (USB Audio Device)".into(),
        mic_enabled: false,
        voice_app: "chat.exe".into(),
        app_audio: vec![
            AppAudioSource { exe: "player.exe".into(), label: "Music".into() },
            AppAudioSource { exe: "CHAT".into(), label: "again".into() },
            AppAudioSource { exe: "browser.exe".into(), label: String::new() },
        ],
        ..RecordingCfg::default()
    };
    let list = from_legacy(&rec);
    assert_eq!(ids(&list), ["game", "mic", "voice", "app-1", "app-3"], "the voice app is not listed twice");
    assert_eq!(list[0].device, "Speakers (USB Audio Device)", "the desktop device moves to the game source");
    assert_eq!((list[1].device.as_str(), list[1].enabled), ("Microphone (USB Audio Device)", false));
    assert_eq!((list[2].kind.as_str(), list[2].exe.as_str(), list[2].name.as_str()), ("app", "chat.exe", "Voice chat"));
    assert_eq!((list[3].name.as_str(), list[4].name.as_str()), ("Music", "browser"), "label, else the exe stem");
    assert!(validate(&list).is_ok());
}

#[test]
fn a_loaded_config_without_the_list_gets_one_and_a_saved_list_is_kept() {
    let mut rec = RecordingCfg { mic_device: "Microphone (USB Audio Device)".into(), audio_sources: Vec::new(), ..RecordingCfg::default() };
    migrate_loaded(&mut rec, "[recording]\nbuffer_secs = 60\n");
    assert_eq!(rec.audio_sources[1].device, "Microphone (USB Audio Device)");
    let mut kept = RecordingCfg { audio_sources: vec![source("only", "output", "Desktop")], ..RecordingCfg::default() };
    migrate_loaded(&mut kept, "[[recording.audio_sources]]\nid = \"only\"\nkind = \"output\"\nname = \"Desktop\"\n");
    assert_eq!(ids(&kept.audio_sources), ["only"]);
}

#[test]
fn bad_lists_are_rejected() {
    let ok = default_sources();
    let with = |edit: &dyn Fn(&mut Vec<AudioSource>)| {
        let mut list = ok.clone();
        edit(&mut list);
        validate(&list).unwrap_err()
    };
    assert!(with(&|l| l[1].id = "GAME".into()).contains("unique"));
    assert!(with(&|l| l[1].kind = "bus".into()).contains("kind"));
    assert!(with(&|l| l[1].name = " ".into()).contains("name"));
    assert!(with(&|l| l[1].gain_db = 13.0).contains("gain_db"));
    assert!(with(&|l| l[1].gain_db = -61.0).contains("gain_db"));
    assert!(with(&|l| l.push(source("app", "app", "App"))).contains("exe"));
    assert!(with(&|l| l.push(AudioSource { exe: r"C:\apps\a.exe".into(), ..source("app", "app", "App") })).contains("exe"));
    assert!(with(&|l| l.push(source("game2", "game", "Game 2"))).contains("one game"));
    assert!(with(&|l| l.extend((0..7).map(|n| source(&format!("s{n}"), "input", "Input")))).contains("at most 8"));
    assert!(validate(&[]).is_ok(), "no sources = clips without audio");
}

#[test]
fn the_engine_gets_enabled_sources_in_order_and_a_disabled_one_changes_tracks() {
    let mut list = default_sources();
    list.push(AudioSource { exe: " player.exe ".into(), gain_db: -6.0, ..source("app-1", "app", " Music ") });
    list[1].enabled = false;
    let value: Vec<AudioSource> = serde_json::from_str(&engine_value(&list)).unwrap();
    assert_eq!(ids(&value), ["game", "app-1"]);
    assert_eq!((value[1].exe.as_str(), value[1].name.as_str(), value[1].gain_db), ("player.exe", "Music", -6.0));
    assert_eq!(engine_value(&[]), "[]");
    assert!(changed(&default_sources(), &list));
    let mut renamed = default_sources();
    renamed[0].name = "Game audio".into();
    assert!(!changed(&default_sources(), &renamed), "a name is applied live, no restart");
    assert_eq!(json_lines(&live_updates(&default_sources(), &renamed)), vec![serde_json::from_str::<serde_json::Value>(r#"{"cmd":"set_source","id":"game","name":"Game audio"}"#).unwrap()]);
    let mut disabled_twice = list.clone();
    disabled_twice[1].gain_db = 9.0;
    assert!(!changed(&list, &disabled_twice), "a disabled source records nothing either way");
}

#[test]
fn gain_and_names_go_live_and_everything_else_restarts() {
    let base = default_sources();
    let mut louder = base.clone();
    louder[1].gain_db = -4.5;
    assert!(!changed(&base, &louder), "a fader move keeps the replay buffer");
    assert_eq!(json_lines(&live_updates(&base, &louder)), vec![serde_json::from_str::<serde_json::Value>(r#"{"cmd":"set_source","gain_db":-4.5,"id":"mic"}"#).unwrap()]);
    let mut both = louder.clone();
    both[1].name = " Voice ".into();
    assert_eq!(json_lines(&live_updates(&base, &both)), vec![serde_json::from_str::<serde_json::Value>(r#"{"cmd":"set_source","gain_db":-4.5,"id":"mic","name":"Voice"}"#).unwrap()]);
    assert!(live_updates(&base, &base).is_empty());
    let mut device = base.clone();
    device[1].device = "Microphone (USB Audio Device)".into();
    assert!(changed(&base, &device), "another device: restart");
    let mut reordered = base.clone();
    reordered.swap(0, 1);
    assert!(changed(&base, &reordered), "track order is the clip's: restart");
    let mut off = base.clone();
    off[1].enabled = false;
    assert!(changed(&base, &off), "a track fewer: restart");
    let mut off_and_louder = off.clone();
    off_and_louder[1].gain_db = 3.0;
    assert!(live_updates(&off, &off_and_louder).is_empty(), "a disabled source has no live track");
}
