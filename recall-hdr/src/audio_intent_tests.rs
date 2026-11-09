//! design-audio-mute §3.3 P8 and P10: the restatement and the live change lines. Made-up names.

use serde_json::{json, Value};

use super::*;

fn src(id: &str, kind: &str, device: &str, mic_mute: Option<bool>) -> AudioSource {
    AudioSource { id: id.into(), kind: kind.into(), name: id.into(), device: device.into(), mic_mute, ..AudioSource::default() }
}

fn ids_retired(lines: &[Value]) -> Vec<&str> {
    lines.iter().filter(|l| l["retire"] == json!(true)).filter_map(|l| l["id"].as_str()).collect()
}

/// B2: a line-in's flag dropped (Some(false) -> None, i.e. muted) is a live `mic_mute: true`, not
/// only a restart; and it is no shape change any more.
#[test]
fn a_dropped_flag_goes_live_as_muted() {
    let old = vec![src("line", "input", "Line (Box)", Some(false))];
    let new = vec![src("line", "input", "Line (Box)", None)];
    assert_eq!(change_lines(&old, &new), vec![json!({"cmd": "set_source", "id": "line", "mic_mute": true})]);
    assert!(!crate::record_audio_sources::changed(&old, &new), "no restart for it");
}

/// B3: a removed, a disabled and a re-picked (other name) source are retired live.
#[test]
fn removed_disabled_and_renamed_devices_are_retired_live() {
    let old = vec![src("mic", "input", "Mic (Desk)", None), src("line", "input", "Line (Box)", Some(false)), src("out", "output", "Speakers (Pod)", None)];
    let mut new = vec![src("mic", "input", "Mic (Other)", None), src("line", "input", "Line (Box)", Some(false))];
    new[1].enabled = false;
    assert_eq!(ids_retired(&change_lines(&old, &new)), vec!["mic", "line", "out"]);
    assert!(crate::record_audio_sources::changed(&old, &new), "the shape restart still follows");
}

/// P8: the restatement carries every current source's full state (effective flag, saved id) and
/// retires a baked track the current list no longer records the same way.
#[test]
fn the_restatement_states_every_source_and_retires_what_moved() {
    let baked = vec![src("mic", "input", "Mic (Desk)", None), src("gone", "output", "Pod", None)];
    let mut mic = src("mic", "input", "Mic (Desk)", Some(false));
    mic.device_id = "{A}".into();
    let lines = restatement(&baked, &[mic]);
    assert_eq!(lines[0]["mic_mute"], json!(false));
    assert_eq!(lines[0]["device_id"], json!("{A}"));
    assert_eq!(ids_retired(&lines), vec!["gone"]);
    let none = restatement(&baked, &[src("mic", "input", "Mic (Desk)", None)]);
    assert_eq!(none[0]["mic_mute"], json!(true), "no flag: muted by Alt+M");
}

#[test]
fn a_line_gets_its_seq() {
    let line: Value = serde_json::from_str(&stamped(r#"{"cmd":"set_mic_mute","muted":true}"#, 7)).unwrap();
    assert_eq!(line, json!({"cmd": "set_mic_mute", "muted": true, "seq": 7}));
}
