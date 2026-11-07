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
    assert_eq!(strip_note(&saved, Some(&fallback)).as_deref(), Some(OPENED_OTHER));
    let own = lists(&[("Mic (Box)", "{id-one}", Some(2)), ("Speakers (Box)", "{id-spk}", None)]);
    let with_id = line_in("Mic (Box)", "{id-one}");
    assert!(!mutes(&with_id, Some(&own)), "exact name and matching id: live");
    assert_eq!(strip_note(&with_id, Some(&own)), None);
    assert!(!mutes(&saved, Some(&own)), "an old config without an id: an exact-name open is live");
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
    assert_eq!(mic_row(&line_in("Mic (Box)", "")), (false, Some(true)), "a named line-in: off, a click turns it on");
    let mic = Source { mic_mute: None, ..line_in("Mic (Box)", "") };
    assert_eq!(mic_row(&mic), (true, Some(false)), "no flag: muted, a click makes it a line-in");
}

/// 2026-11-07 audit R5-deep-audio-2: a line-in moved onto an endpoint Windows lists as a
/// microphone turns Alt+M back on; a line-level one, a microphone strip or no data change nothing.
#[test]
fn moving_a_line_in_onto_a_microphone_turns_the_mic_hotkey_back_on() {
    let l = lists(&[("Mic (Box)", "{a}", Some(4)), ("Line (Box)", "{b}", Some(2)), ("Headset (Box)", "{c}", Some(5)), ("Odd (Box)", "{d}", None)]);
    let source = line_in("Line (Box)", "{b}");
    assert_eq!(device_pick_effect(&source, Some(&l), Some(0)), Some(true));
    assert_eq!(device_pick_effect(&source, Some(&l), Some(2)), Some(true), "a headset is a microphone");
    assert_eq!(device_pick_effect(&source, Some(&l), Some(1)), None, "line-level: no change");
    assert_eq!(device_pick_effect(&source, Some(&l), Some(3)), None, "unknown: no change");
    assert_eq!(device_pick_effect(&source, Some(&l), None), None, "the Windows default row: no change");
    let mic = Source { mic_mute: Some(true), ..source.clone() };
    assert_eq!(device_pick_effect(&mic, Some(&l), Some(0)), None, "already muted");
}

/// 2026-11-07 audit R5-deep-audio-5: the meter of a source Alt+M mutes shows silence while it is on.
#[test]
fn the_meter_is_flat_while_the_mic_hotkey_mutes_the_source() {
    assert_eq!(shown_level(-12.0, true, true), -90.0, "muted and muting: what the clip records");
    assert_eq!(shown_level(-12.0, true, false), -12.0, "a line-in that skips Alt+M keeps moving");
    assert_eq!(shown_level(-12.0, false, true), -12.0, "not muted");
}
