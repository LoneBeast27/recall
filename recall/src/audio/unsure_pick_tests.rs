//! A4 ruling (merge gate p9): two or more endpoints share the saved name and no saved id picks
//! one. An exact-name input sibling opens (muted, `hold.rs`); an output waits; a name that
//! matches only in case waits and says so (P2-d). Made-up names and ids, no WASAPI.

use super::{choose, choose_endpoint_by, Choice, Pick};

fn names(present: &[(&str, &str)]) -> Vec<String> {
    present.iter().map(|(n, _)| (*n).to_owned()).collect()
}

/// Saved "Mic (Box)" with id A; connected "Mic (Box)" (id B) and "2- Mic (Box)" (id C).
const SUBSTITUTES: [(&str, &str); 2] = [("Mic (Box)", "{B}"), ("2- Mic (Box)", "{C}")];

fn pick(present: &[(&str, &str)], input: bool) -> Result<Pick, String> {
    choose_endpoint_by(&names(present), (Some("Mic (Box)"), "{A}"), (false, input), |i| Some(present[i].1.to_owned()))
}

#[test]
fn an_exact_name_sibling_without_an_id_match_is_unsure() {
    let got = choose(&names(&SUBSTITUTES), "Mic (Box)", "{A}", |i| Some(SUBSTITUTES[i].1.to_owned()));
    assert_eq!(got, Choice::Unsure { index: 0, twins: 2 });
    let none = choose(&names(&SUBSTITUTES), "Mic (Box)", "", |_| panic!("no saved id: no id is read"));
    assert_eq!(none, Choice::Unsure { index: 0, twins: 2 }, "no saved id matches either");
}

#[test]
fn an_input_opens_the_unsure_sibling_and_an_output_waits_for_the_original() {
    assert_eq!(pick(&SUBSTITUTES, true), Ok(Pick::Unsure(0)), "a mic or line-in opens B (muted)");
    let output = [("Speakers (Box)", "{B}"), ("2- Speakers (Box)", "{C}")];
    let got = choose_endpoint_by(&names(&output), (Some("Speakers (Box)"), "{A}"), (false, false), |i| Some(output[i].1.to_owned()));
    assert_eq!(got, Err("Speakers (Box): 2 devices share this name; waiting for the original".to_owned()));
}

#[test]
fn the_saved_id_still_opens_its_own_device_clearly() {
    let own = [("Mic (Box)", "{B}"), ("2- Mic (Box)", "{A}")];
    assert_eq!(pick(&own, true), Ok(Pick::Named(1)), "the id picks own: not unsure");
    assert_eq!(pick(&[("Mic (Box)", "{B}")], true), Ok(Pick::Named(0)), "one endpoint by the name: as before");
}

#[test]
fn twins_without_the_exact_name_wait_with_the_notice() {
    let two = [("4- Mic (Box)", "{B}"), ("5- Mic (Box)", "{C}")];
    let got = choose_endpoint_by(&names(&two), (Some("3- Mic (Box)"), "{A}"), (false, true), |i| Some(two[i].1.to_owned()));
    assert_eq!(got, Err("3- Mic (Box): 2 devices share this name; waiting for the original".to_owned()));
}

#[test]
fn a_name_that_matches_only_in_case_says_pick_it_again() {
    let present = [("mic (box)", "{A}")];
    let got = choose_endpoint_by(&names(&present), (Some("Mic (Box)"), "{A}"), (false, true), |i| Some(present[i].1.to_owned()));
    assert_eq!(got, Err("Mic (Box) not found exactly; pick it again".to_owned()));
    let renumbered = [("4- MIC (BOX)", "{A}")];
    let got = choose_endpoint_by(&names(&renumbered), (Some("3- Mic (Box)"), ""), (false, false), |_| None);
    assert_eq!(got, Err("3- Mic (Box) not found exactly; pick it again".to_owned()), "renumbered too");
    let other = [("Speakers (Box)", "{A}")];
    assert_eq!(choose_endpoint_by(&names(&other), (Some("Mic (Box)"), ""), (false, true), |_| None), Err("'Mic (Box)' is not connected".to_owned()));
}
