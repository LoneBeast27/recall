//! Merge gate p9: P1-B (an endpoint id saved for the same device name goes live, no restart),
//! P2-a (an older engine is restarted for fields it ignores, never sent them) and the A4
//! ruling's adoption patch. Made-up device names and ids.

use super::*;
use crate::record_config::RecordingCfg;
use crate::record_engine::engine_settings_changed;

fn source(json: serde_json::Value) -> AudioSource {
    serde_json::from_value(json).unwrap()
}

fn rec(list: Vec<AudioSource>) -> RecordingCfg {
    RecordingCfg { audio_sources: list, ..RecordingCfg::default() }
}

/// A config saved before endpoint ids: a line-in strip with no id.
fn legacy() -> AudioSource {
    source(serde_json::json!({"id": "in-1", "kind": "input", "name": "Line-in", "device": "Line (Box)"}))
}

#[test]
fn unmuting_or_repicking_the_same_named_device_restarts_no_engine() {
    let old = rec(vec![legacy()]);
    // Unticking "Muted by the mic hotkey": mic_mute false and the endpoint id written.
    let unmuted = rec(vec![AudioSource { mic_mute: Some(false), device_id: "{id-one}".into(), ..legacy() }]);
    assert!(!engine_settings_changed(&old, &unmuted), "unmuting keeps the replay buffer");
    // "Pick this device again": the same name with its id.
    let repicked = rec(vec![AudioSource { device_id: "{id-one}".into(), ..legacy() }]);
    assert!(!engine_settings_changed(&old, &repicked), "re-picking the same name keeps the buffer");
    let lines = crate::audio_intent::change_lines(&old.audio_sources, &unmuted.audio_sources);
    let sent = lines[0].clone();
    assert_eq!(sent, serde_json::json!({"cmd": "set_source", "id": "in-1", "mic_mute": false, "device_id": "{id-one}"}));
    // Another device name is another track: that still restarts.
    let moved = rec(vec![AudioSource { device: "Mic (Desk)".into(), ..legacy() }]);
    assert!(engine_settings_changed(&old, &moved));
}

#[test]
fn an_engine_without_the_fields_capability_is_restarted_for_them() {
    let flag = vec![r#"{"cmd":"set_source","id":"in-1","mic_mute":false}"#.to_owned()];
    let id = vec![r#"{"cmd":"set_source","id":"in-1","device_id":"{a}"}"#.to_owned()];
    let boost = vec![r#"{"cmd":"set_source","id":"game","gain_db":3.0}"#.to_owned()];
    let cut = vec![r#"{"cmd":"set_source","id":"game","gain_db":-3.0,"name":"Game"}"#.to_owned()];
    for lines in [&flag, &id, &boost] {
        assert!(cannot_take(lines, true, false), "8111e31 ignores {lines:?}: restart");
        assert!(!cannot_take(lines, true, true), "a new engine takes {lines:?} live");
    }
    assert!(!cannot_take(&cut, true, false), "a cut gain and a name: any live engine");
    assert!(cannot_take(&cut, false, false), "no live sources at all: restart");
    assert!(!cannot_take(&[], false, false), "nothing to send");
}

/// P1-b: a line-in (`mic_mute` false) never adopts an endpoint id (it would skip Alt+M from
/// then on): the user picks the device on the Audio page instead.
#[test]
fn a_line_in_never_adopts_an_endpoint_id() {
    let list = vec![AudioSource { mic_mute: Some(false), device_id: "{A}".into(), ..legacy() }];
    let ev = serde_json::json!({"type": "source_adopted", "id": "in-1", "device_id": "{B}"});
    assert_eq!(adopt_patch(&list, &ev), None, "a line-in's id is never adopted");
    let mic = vec![AudioSource { mic_mute: Some(true), device_id: "{A}".into(), ..legacy() }];
    assert_eq!(adopt_patch(&mic, &ev).expect("a microphone adopts")["recording"]["audio_sources"][0]["device_id"], "{B}");
}

#[test]
fn an_adopted_endpoint_id_is_saved_as_the_sources_own() {
    let list = vec![AudioSource { device_id: "{A}".into(), ..legacy() }];
    let ev = serde_json::json!({"type": "source_adopted", "id": "in-1", "device_id": "{B}"});
    let patch = adopt_patch(&list, &ev).expect("a patch");
    assert_eq!(patch["recording"]["audio_sources"][0]["device_id"], "{B}");
    assert_eq!(patch["recording"]["audio_sources"][0]["device"], "Line (Box)", "the device name stays");
    let saved = vec![AudioSource { device_id: "{B}".into(), ..legacy() }];
    assert_eq!(adopt_patch(&saved, &ev), None, "already its own");
    assert_eq!(adopt_patch(&list, &serde_json::json!({"type": "saved"})), None);
    assert_eq!(adopt_patch(&list, &serde_json::json!({"type": "source_adopted", "id": "nope", "device_id": "{B}"})), None);
    assert_eq!(adopt_patch(&list, &serde_json::json!({"type": "source_adopted", "id": "in-1", "device_id": "{B}\n"})).map(|_| ()), Some(()), "outer whitespace trimmed");
    assert_eq!(adopt_patch(&list, &serde_json::json!({"type": "source_adopted", "id": "in-1", "device_id": "a\u{7}b"})), None, "malformed");
    assert!(crate::supervisor::FORWARDED_EVENTS.contains(&"source_adopted"), "the supervisor hands it to the controller");
}
