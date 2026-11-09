//! A4 amendment (merge gate, 08:29 UTC): the page copy of the endpoint-id tie-break. Made-up
//! names and ids, no WASAPI. The same table as the recorder's (`recall` `audio/id_pick_tests.rs`),
//! so the meter listens to the endpoint the recorder records.

use super::pick_endpoint;
use std::cell::Cell;

/// `pick_endpoint` over `present` (name, id), and how many endpoint ids it read.
fn pick(want: &str, want_id: &str, present: &[(&str, &str)]) -> (Option<usize>, usize) {
    let names: Vec<String> = present.iter().map(|(n, _)| (*n).to_owned()).collect();
    let reads = Cell::new(0);
    let picked = pick_endpoint(&names, want, want_id, |i| {
        reads.set(reads.get() + 1);
        Some(present[i].1.to_owned())
    });
    (picked, reads.get())
}

#[test]
fn the_own_device_back_renumbered_beside_a_same_named_substitute_is_picked_by_its_id() {
    let present = [("4- Mic (Box)", "{sub}"), ("Speakers (Box)", "{spk}"), ("5- Mic (Box)", "{own}")];
    assert_eq!(pick("3- Mic (Box)", "{own}", &present).0, Some(2), "renumbered own beside a renumbered substitute");
    let present = [("Mic (Box)", "{sub}"), ("2- Mic (Box)", "{own}")];
    assert_eq!(pick("Mic (Box)", "{own}", &present).0, Some(1), "the substitute took the exact name: the id still picks own");
    assert_eq!(pick("Mic (Box)", " {own} ", &present).0, Some(1), "outer spaces of the saved id do not count");
}

#[test]
fn exact_name_twins_are_told_apart_by_the_saved_id() {
    let twins = [("Mic (Box)", "{sub}"), ("Mic (Box)", "{own}")];
    assert_eq!(pick("Mic (Box)", "{own}", &twins).0, Some(1));
    assert_eq!(pick("Mic (Box)", "{sub}", &twins).0, Some(0));
}

#[test]
fn two_same_named_devices_and_no_id_match_wait() {
    let two = [("4- Mic (Box)", "{a}"), ("5- Mic (Box)", "{b}")];
    assert_eq!(pick("3- Mic (Box)", "{own}", &two).0, None, "the saved id is neither: wait");
    assert_eq!(pick("3- Mic (Box)", "", &two), (None, 0), "no saved id: wait, and no id is read");
    assert_eq!(pick("3- Mic (Box)", "{a}\nx", &two), (None, 0), "a malformed saved id counts as none");
    let same_id = [("4- Mic (Box)", "{own}"), ("5- Mic (Box)", "{own}")];
    assert_eq!(pick("3- Mic (Box)", "{own}", &same_id).0, None, "the id matches both: wait");
    let twins = [("Mic (Box)", "{a}"), ("Mic (Box)", "{b}")];
    assert_eq!(pick("Mic (Box)", "{own}", &twins).0, None, "exact twins, no id match: wait");
}

#[test]
fn an_id_match_whose_name_differs_never_opens() {
    assert_eq!(pick("Mic (Box)", "{own}", &[("Line (Desk)", "{own}")]).0, None, "only the id matches");
    let present = [("Mic (Box)", "{a}"), ("2- Mic (Box)", "{b}"), ("Line (Desk)", "{own}")];
    assert_eq!(pick("3- Mic (Box)", "{own}", &present).0, None, "two name matches, the id is on a third endpoint");
    assert_eq!(pick("Mic (Box)", "{own}", &[("mic (box)", "{own}")]).0, None, "case included");
}

#[test]
fn one_name_match_keeps_the_rule_without_reading_ids() {
    assert_eq!(pick("3- Mic (Box)", "{own}", &[("4- Mic (Box)", "{other}")]), (Some(0), 0), "the one match opens; Alt+M decides by id");
    assert_eq!(pick("Mic (Box)", "{own}", &[("Speakers (Box)", "{own}")]), (None, 0), "no name match: wait");
    // Today's rule when the id decides nothing: one exact name beside a renumbered one opens.
    let present = [("Mic (Box)", "{a}"), ("2- Mic (Box)", "{b}")];
    assert_eq!(pick("Mic (Box)", "{own}", &present).0, Some(0), "no id match: the exact name, as today");
    assert_eq!(pick("Mic (Box)", "", &present), (Some(0), 0), "no saved id: as today, no id read");
}
