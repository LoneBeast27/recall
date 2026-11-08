//! Alt+M on the Audio page, with injected endpoint lists (made-up names and ids).
use super::*;
use crate::services::audio_sources::{mutes, strip_note};

fn lists(inputs: &[(&str, &str, Option<u32>)]) -> DeviceLists {
    DeviceLists {
        inputs: inputs.iter().map(|(n, _, _)| (*n).to_owned()).collect(),
        input_ids: inputs.iter().map(|(_, id, _)| (*id).to_owned()).collect(),
        input_forms: inputs.iter().map(|(_, _, f)| *f).collect(),
        ..DeviceLists::default()
    }
}

fn line_in(device: &str, device_id: &str) -> Source {
    Source { id: "line-in".into(), kind: Kind::Input, device: device.into(), device_id: device_id.into(), exe: String::new(), name: "Line-in".into(), enabled: true, gain_db: 0.0, mic_mute: Some(false) }
}

/// 2026-11-07 audit R5-deep-audio-1: a line-in skips Alt+M only on its own saved endpoint.
#[test]
fn a_line_in_on_another_endpoint_follows_the_mic_hotkey() {
    let fallback = lists(&[("2- Mic (Box)", "{id-two}", Some(4))]);
    let saved = line_in("Mic (Box)", "");
    assert!(mutes(&saved, Some(&fallback)), "\"Mic (Box)\" saved, only \"2- Mic (Box)\" present: muted by Alt+M");
    assert_eq!(strip_note(&saved, Some(&fallback)).as_deref(), Some(NO_ID), "no id saved (R6-deep-audio-1)");
    let own = lists(&[("Mic (Box)", "{id-one}", Some(2)), ("Speakers (Box)", "{id-spk}", None)]);
    let with_id = line_in("Mic (Box)", "{id-one}");
    assert!(!mutes(&with_id, Some(&own)), "exact name and matching id: live");
    assert_eq!(strip_note(&with_id, Some(&own)), None);
    assert!(mutes(&saved, Some(&own)), "no id saved, exact name: muted (R6-deep-audio-1)");
    let twin = lists(&[("Mic (Box)", "{id-two}", Some(2))]);
    assert!(mutes(&with_id, Some(&twin)), "exact name, another id (a twin took the name): muted");
    assert_eq!(strip_note(&with_id, Some(&twin)).as_deref(), Some(OPENED_OTHER));
    assert!(!mutes(&with_id, None), "lists not read yet: the strip's flag");
    let mic = Source { mic_mute: Some(true), ..saved.clone() };
    assert!(mutes(&mic, Some(&own)) && strip_note(&mic, Some(&fallback)).is_none(), "a microphone: muted, no note");
}

/// 2026-11-07 audit R5-deep-audio-3: on a source with no device the row is checked and inert.
#[test]
fn the_mic_hotkey_row_is_inert_without_a_device() {
    assert_eq!(mic_row(&line_in("", "")), (true, None), "the Windows default: always muted, nothing to flip");
    assert_eq!(mic_row(&line_in("Mic (Box)", "{a}")), (false, Some(true)), "a named line-in: off, a click turns it on");
    let mic = Source { mic_mute: None, ..line_in("Mic (Box)", "") };
    assert_eq!(mic_row(&mic), (true, Some(false)), "no flag: muted, a click makes it a line-in");
}

/// 2026-11-07 audit R5-deep-audio-2: a line-in moved onto an endpoint Windows lists as a
/// microphone turns Alt+M back on; a line-level one, a microphone strip or no data change nothing.
#[test]
fn moving_a_line_in_onto_a_microphone_turns_the_mic_hotkey_back_on() {
    let l = lists(&[("Mic (Box)", "{a}", Some(4)), ("Line (Box)", "{b}", Some(2)), ("Headset (Box)", "{c}", Some(5)), ("Odd (Box)", "{d}", None)]);
    let source = line_in("Line (Box)", "{b}");
    assert_eq!(device_pick_effect(&source, Some(&l), ("Mic (Box)", "{a}")), Some(true));
    assert_eq!(device_pick_effect(&source, Some(&l), ("Headset (Box)", "{c}")), Some(true), "a headset is a microphone");
    assert_eq!(device_pick_effect(&source, Some(&l), ("Line (Box)", "{b}")), None, "line-level: no change");
    assert_eq!(device_pick_effect(&source, Some(&l), ("Odd (Box)", "{d}")), None, "unknown: no change");
    assert_eq!(device_pick_effect(&source, Some(&l), ("", "")), None, "the Windows default row: no change");
    let mic = Source { mic_mute: Some(true), ..source.clone() };
    assert_eq!(device_pick_effect(&mic, Some(&l), ("Mic (Box)", "{a}")), None, "already muted");
}

/// 2026-11-07 audit R5-deep-audio-5: the meter of a source Alt+M mutes shows silence while it is on.
#[test]
fn the_meter_is_flat_while_the_mic_hotkey_mutes_the_source() {
    assert_eq!(shown_level(-12.0, true, true), -90.0, "muted and muting: what the clip records");
    assert_eq!(shown_level(-12.0, true, false), -12.0, "a line-in that skips Alt+M keeps moving");
    assert_eq!(shown_level(-12.0, false, true), -12.0, "not muted");
}

/// 2026-11-07 audit R6-deep-audio-1/2: the saved id alone decides; no id follows Alt+M.
#[test]
fn the_saved_id_alone_decides_whether_a_line_in_skips_the_mic_hotkey() {
    let own = lists(&[("Mic (Box)", "{id-one}", Some(2))]);
    assert!(mutes(&line_in("Mic (Box)", ""), Some(&own)), "no id + exact name: muted");
    assert!(mutes(&line_in("Mic (Box)", ""), None), "no id, lists not read: muted");
    let renamed = lists(&[("4- Line (Box)", "{a}", Some(2))]);
    assert!(!mutes(&line_in("3- Line (Box)", "{a}"), Some(&renamed)), "id match, renamed \"4- Line (Box)\": live");
    assert_eq!(strip_note(&line_in("3- Line (Box)", "{a}"), Some(&renamed)), None);
    let other = lists(&[("4- Line (Box)", "{b}", Some(2))]);
    assert!(mutes(&line_in("3- Line (Box)", "{a}"), Some(&other)), "id mismatch: muted");
}

/// 2026-11-07 audit R6-deep-audio-1: unticking saves the endpoint id of the unique exact match;
/// with no readable id the untick is refused.
#[test]
fn unticking_the_mic_hotkey_saves_the_endpoint_id() {
    let l = lists(&[("Mic (Box)", "{id-one}", Some(2)), ("Line (Box)", "", Some(2))]);
    let mic = |device: &str| Source { mic_mute: Some(true), ..line_in(device, "") };
    assert_eq!(mic_toggle_effect(&mic("Mic (Box)"), Some(&l)), Ok((false, "{id-one}".to_owned())));
    assert!(mic_toggle_effect(&mic("Mic (Box)"), None).is_err(), "lists not read: refused");
    assert!(mic_toggle_effect(&mic("Line (Box)"), Some(&l)).is_err(), "no readable id: refused");
    assert!(mic_toggle_effect(&mic("2- Mic (Box)"), Some(&l)).is_err(), "only a renumbered match: refused");
    assert!(mic_toggle_effect(&mic("Gone (Box)"), Some(&l)).is_err(), "not connected: refused");
    let renamed = lists(&[("4- Line (Box)", "{a}", Some(2))]);
    let with_id = Source { mic_mute: Some(true), ..line_in("3- Line (Box)", "{a}") };
    assert_eq!(mic_toggle_effect(&with_id, Some(&renamed)), Ok((false, "{a}".to_owned())), "the saved id, renumbered: kept");
    let gone = Source { device_id: "{gone}".into(), ..mic("Mic (Box)") };
    assert!(mic_toggle_effect(&gone, Some(&l)).is_err(), "the saved id is not listed: refused, never another endpoint's id");
    let twins = lists(&[("Mic (Box)", "{a}", Some(2)), ("Mic (Box)", "{b}", Some(2))]);
    assert!(mic_toggle_effect(&mic("Mic (Box)"), Some(&twins)).is_err(), "twins: refused");
}

/// 2026-11-07 audit R6-deep-audio-5: a hand-edited id that is not one line of at most 256
/// characters is read as no id (the source stays, and follows Alt+M) and is never saved back.
#[test]
fn a_malformed_device_id_is_read_as_none() {
    use crate::services::audio_sources::{from_config, to_config};
    let value = serde_json::json!([{"id": "line-in", "kind": "input", "name": "Line-in", "device": "Mic (Box)", "device_id": "a\nb", "mic_mute": false}]);
    let list = from_config(Some(&value));
    assert_eq!(list.len(), 1, "the source stays");
    assert_eq!(list[0].device_id, "", "read as no id");
    let long = Source { device_id: "x".repeat(300), ..line_in("Mic (Box)", "") };
    assert!(mutes(&long, Some(&lists(&[("Mic (Box)", &"x".repeat(300), Some(2))]))), "too long: no id, muted");
    assert!(!to_config(&[long]).to_string().contains("device_id"), "never saved back");
}

/// 2026-11-07 audit R6-deep-audio-4: the re-arm reads the picked device in the lists at click
/// time. Rendered: [Line (Box), Mic (Box)]; refreshed before the click: [Mic (Box), Line (Box)].
#[test]
fn the_pick_finds_the_device_in_the_lists_at_click_time() {
    let now = lists(&[("Mic (Box)", "{a}", Some(4)), ("Line (Box)", "{b}", Some(2))]);
    let source = line_in("Line (Box)", "{b}");
    assert_eq!(device_pick_effect(&source, Some(&now), ("Mic (Box)", "{a}")), Some(true), "Mic (Box) picked (index 1 at render)");
    assert_eq!(device_pick_effect(&source, Some(&now), ("Line (Box)", "{b}")), None, "Line (Box) picked (index 0 at render)");
    assert_eq!(device_pick_effect(&source, Some(&now), ("2- Mic (Box)", "")), Some(true), "no id: found by the name the recorder matches");
}

/// 2026-11-07 audit R7-deep-audio-5: a named line-in with no saved id IS muted by Alt+M, so its
/// row shows checked (with the `NO_ID` note); a click unticks it, which saves the id.
#[test]
fn a_line_in_without_an_id_shows_the_mic_hotkey_row_checked() {
    assert_eq!(mic_row(&line_in("Mic (Box)", "")), (true, Some(false)), "muted: checked, a click unticks");
    assert_eq!(mic_row(&line_in("Mic (Box)", "x\u{7}")), (true, Some(false)), "a malformed id is no id");
    assert_eq!(other_note(&line_in("Mic (Box)", ""), None), Some(NO_ID), "with its note");
}

/// 2026-11-07 audit R7-deep-audio-1 (page): with no saved id, the untick never guesses between
/// identical dongles. Windows gave "Mic (Box)" to the headset dongle and "2- Mic (Box)" to the
/// line-in one: refused, the user re-picks from the list (which saves the picked endpoint's id).
#[test]
fn unticking_is_refused_when_an_identical_device_shares_the_name() {
    let mic = |device: &str| Source { mic_mute: Some(true), ..line_in(device, "") };
    let twins = lists(&[("Mic (Box)", "{h}", Some(2)), ("2- Mic (Box)", "{c}", Some(2))]);
    assert_eq!(mic_toggle_effect(&mic("Mic (Box)"), Some(&twins)), Err(CONFIRM_PICK), "a sibling with the same stripped name: refused");
    assert!(CONFIRM_PICK.contains("Pick this device again to confirm it"));
    let one = lists(&[("Mic (Box)", "{h}", Some(2)), ("Speakers (Box)", "{s}", None)]);
    assert_eq!(mic_toggle_effect(&mic("Mic (Box)"), Some(&one)), Ok((false, "{h}".to_owned())), "the only one: its id");
    let with_id = Source { device_id: "{c}".into(), ..mic("2- Mic (Box)") };
    assert_eq!(mic_toggle_effect(&with_id, Some(&twins)), Ok((false, "{c}".to_owned())), "a saved id is not a guess: kept");
}
