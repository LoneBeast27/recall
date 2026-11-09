//! A4 amendment (merge gate, 08:29 UTC) on the Audio page: the strip, its note, the meter and
//! Alt+M follow the endpoint the recorder picks with the endpoint-id tie-break. Made-up names
//! and ids, no WASAPI.
use super::*;
use crate::services::audio_sources::{mutes, strip_note, Kind};
use crate::services::meters::{meter_pick_id, spec_for, DevicePick, MeterTarget};

fn lists(inputs: &[(&str, &str)]) -> DeviceLists {
    DeviceLists {
        inputs: inputs.iter().map(|(n, _)| (*n).to_owned()).collect(),
        input_ids: inputs.iter().map(|(_, id)| (*id).to_owned()).collect(),
        input_forms: vec![None; inputs.len()],
        ..DeviceLists::default()
    }
}

fn input(device: &str, device_id: &str, mic: bool) -> Source {
    Source { communications: false, icon: String::new(), id: "in-1".into(), kind: Kind::Input, device: device.into(), device_id: device_id.into(), exe: String::new(), name: "In".into(), enabled: true, gain_db: 0.0, mic_mute: Some(mic) }
}

const RENUMBERED: [(&str, &str); 3] = [("4- Line (Box)", "{sub}"), ("Speakers (Box)", "{spk}"), ("5- Line (Box)", "{own}")];
const TWINS: [(&str, &str); 2] = [("Line (Box)", "{sub}"), ("Line (Box)", "{own}")];
const NO_MATCH: [(&str, &str); 2] = [("4- Line (Box)", "{a}"), ("5- Line (Box)", "{b}")];
const ID_ELSEWHERE: [(&str, &str); 1] = [("Line (Desk)", "{own}")];

#[test]
fn the_own_device_among_same_named_ones_shows_as_recorded() {
    let renumbered = lists(&RENUMBERED);
    let own = input("3- Line (Box)", "{own}", false);
    assert_eq!(source_found(&own, Some(&renumbered)), Found::Opens(2));
    assert_eq!(source_device_label(&own, Some(&renumbered), None), "5- Line (Box)", "its real name");
    assert_eq!(strip_note(&own, Some(&renumbered)), None, "no \"2 devices match\": it records");
    assert!(!mutes(&own, Some(&renumbered)), "a line-in on its own id: live through Alt+M");
    let twins = lists(&TWINS);
    let own = input("Line (Box)", "{own}", false);
    assert_eq!(source_found(&own, Some(&twins)), Found::Opens(1));
    assert_eq!(strip_note(&own, Some(&twins)), None);
    assert!(!mutes(&own, Some(&twins)));
}

#[test]
fn same_named_devices_without_an_id_match_still_wait() {
    let two = lists(&NO_MATCH);
    let saved = input("3- Line (Box)", "{own}", false);
    assert_eq!(source_found(&saved, Some(&two)), Found::Ambiguous(2));
    assert_eq!(strip_note(&saved, Some(&two)).as_deref(), Some("2 devices match this name"));
    let elsewhere = input("Line (Box)", "{own}", false);
    assert_eq!(source_found(&elsewhere, Some(&lists(&ID_ELSEWHERE))), Found::Missing, "an id match with another name is not connected");
}

#[test]
fn a_muted_mic_stays_muted_in_every_case() {
    for present in [&RENUMBERED[..], &TWINS[..], &NO_MATCH[..], &ID_ELSEWHERE[..]] {
        for saved in ["3- Line (Box)", "Line (Box)"] {
            assert!(mutes(&input(saved, "{own}", true), Some(&lists(present))), "a microphone {saved:?} on {present:?}");
        }
    }
}

#[test]
fn the_meter_listens_to_the_endpoint_the_id_picks() {
    let own = input("3- Line (Box)", " {own} ", false);
    let target = MeterTarget::Device { render: false, device: "3- Line (Box)".into(), device_id: "{own}".into() };
    assert_eq!(spec_for(&own, None).target, target, "the meter carries the saved id");
    let names: Vec<String> = RENUMBERED.iter().map(|(n, _)| (*n).to_owned()).collect();
    let id_of = |i: usize| Some(RENUMBERED[i].1.to_owned());
    assert_eq!(meter_pick_id(&names, "3- Line (Box)", "{own}", id_of), Some(DevicePick::Named(2)));
    assert_eq!(meter_pick_id(&names, "3- Line (Box)", "{none}", id_of), None, "no id match: missing, never the default");
    assert_eq!(meter_pick_id(&names, "Line (Desk)", "{own}", |_| Some("{own}".into())), None, "an id alone opens nothing");
}
