//! The recorder's A4 ruling table (`recall` `audio/unsure_pick_tests.rs`), on the page's copy,
//! and what the strip and the meter make of it. Made-up names and ids.

use super::*;
use crate::services::audio_devices::{device_label, found, DeviceLists, Found};
use crate::services::audio_sources::{strip_note, Kind, Source};
use crate::services::meters::{meter_pick_for, DevicePick};

fn names(present: &[(&str, &str)]) -> Vec<String> {
    present.iter().map(|(n, _)| (*n).to_owned()).collect()
}

/// Saved "Mic (Box)" with id A; connected "Mic (Box)" (id B) and "2- Mic (Box)" (id C).
const SUBSTITUTES: [(&str, &str); 2] = [("Mic (Box)", "{B}"), ("2- Mic (Box)", "{C}")];

#[test]
fn the_ruling_table_matches_the_recorder() {
    let id_of = |i: usize| Some(SUBSTITUTES[i].1.to_owned());
    assert_eq!(choose(&names(&SUBSTITUTES), "Mic (Box)", "{A}", id_of), Choice::Unsure { index: 0, twins: 2 });
    assert_eq!(choose(&names(&SUBSTITUTES), "Mic (Box)", "", |_| panic!("no saved id: no id read")), Choice::Unsure { index: 0, twins: 2 });
    let own = [("Mic (Box)", "{B}"), ("2- Mic (Box)", "{A}")];
    assert_eq!(choose(&names(&own), "Mic (Box)", "{A}", |i| Some(own[i].1.to_owned())), Choice::Opens(1));
    let two = [("4- Mic (Box)", "{B}"), ("5- Mic (Box)", "{C}")];
    assert_eq!(choose(&names(&two), "3- Mic (Box)", "{A}", |i| Some(two[i].1.to_owned())), Choice::Twins(2));
    assert_eq!(choose(&names(&[("mic (box)", "{A}")]), "Mic (Box)", "{A}", |_| None), Choice::CaseOnly);
    assert_eq!(choose(&names(&[("Speakers (Box)", "{A}")]), "Mic (Box)", "", |_| None), Choice::Missing);
}

#[test]
fn an_input_meters_the_unsure_sibling_and_an_output_waits() {
    let list = names(&SUBSTITUTES);
    let id_of = |i: usize| Some(SUBSTITUTES[i].1.to_owned());
    assert_eq!(meter_pick_for(true, &list, ("Mic (Box)", "{A}"), id_of), Some(DevicePick::Named(0)), "the recorder records B (muted)");
    let out = names(&[("Speakers (Box)", ""), ("2- Speakers (Box)", "")]);
    assert_eq!(meter_pick_for(false, &out, ("Speakers (Box)", ""), |_| None), None, "an output twin waits: not metered");
}

fn source(kind: Kind, device: &str, device_id: &str) -> Source {
    Source { communications: false, icon: String::new(), id: "s".into(), kind, device: device.into(), device_id: device_id.into(), exe: String::new(), name: "S".into(), enabled: true, gain_db: 0.0, mic_mute: None }
}

#[test]
fn the_strip_says_what_the_recorder_does() {
    let lists = DeviceLists { inputs: names(&SUBSTITUTES), input_ids: SUBSTITUTES.iter().map(|(_, id)| (*id).to_owned()).collect(), outputs: names(&[("Speakers (Box)", ""), ("2- Speakers (Box)", "")]), ..DeviceLists::default() };
    let mic = source(Kind::Input, "Mic (Box)", "{A}");
    assert_eq!(strip_note(&mic, Some(&lists)).as_deref(), Some("Mic (Box) may be a different device (its ID changed). Started muted; press Alt+M to unmute."));
    let line_in = Source { mic_mute: Some(false), ..source(Kind::Input, "Mic (Box)", "{A}") };
    assert!(crate::services::audio_sources::mutes(&line_in, Some(&lists)), "a line-in on the unsure sibling is not its own device");
    let speakers = source(Kind::Output, "Speakers (Box)", "");
    assert_eq!(strip_note(&speakers, Some(&lists)).as_deref(), Some("Speakers (Box): 2 devices share this name; waiting for the original"));
    let cased = source(Kind::Output, "speakers (box)", "");
    let one = DeviceLists { outputs: vec!["Speakers (Box)".into()], ..DeviceLists::default() };
    assert_eq!(strip_note(&cased, Some(&one)).as_deref(), Some("speakers (box) not found exactly; pick it again"));
    assert_eq!(found("speakers (box)", Some(&one.outputs)), Found::CaseOnly);
    assert_eq!(device_label("speakers (box)", Some(&one.outputs), None), "speakers (box) (not found exactly; pick it again)");
}
