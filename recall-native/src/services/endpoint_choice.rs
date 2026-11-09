//! What a saved source's device picks among the connected endpoints: a copy of the recorder's
//! `choose` (`recall` `audio/endpoints.rs`), so the page meters and describes the endpoint the
//! recorder opens. Keep the two equal (same test table in both).
//!
//! A4 ruling (merge gate p9): when two or more endpoints share the saved name once a leading
//! "<digits>- " index is dropped and no saved id picks one, an exact-name input sibling opens
//! muted until a fresh Alt+M (the recorder then saves its id as the source's own); an output
//! waits for the original. A name that matches only in case waits and says so (P2-d).
use crate::services::meters::{id_ok, match_endpoint, strip_index};

/// The recorder's pick of a saved device.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Choice {
    /// The saved device: the one endpoint by its name, or the one with the saved id among
    /// endpoints that share its name.
    Opens(usize),
    /// Two or more endpoints share the name, no saved id picks one, and this one has exactly the
    /// saved name: an input opens it muted; an output waits.
    Unsure { index: usize, twins: usize },
    /// Two or more endpoints share the name and none has exactly the saved name: it waits.
    Twins(usize),
    /// Only a name in another case, or a short name that is a whole bracketed part of an
    /// endpoint's name: it waits, and says so.
    CaseOnly,
    /// No endpoint has the name.
    Missing,
}

/// The pick of `want` (saved with the endpoint id `want_id`) among `names`; `id_of` reads an
/// endpoint's id by index, only for the tie-break and only with a well-formed saved id.
pub fn choose(names: &[String], want: &str, want_id: &str, id_of: impl Fn(usize) -> Option<String>) -> Choice {
    let want = want.trim();
    if want.is_empty() {
        return Choice::Missing;
    }
    let stripped = strip_index(want);
    let candidates: Vec<usize> = names.iter().enumerate().filter(|(_, n)| strip_index(n) == stripped).map(|(i, _)| i).collect();
    if candidates.len() >= 2 {
        let want_id = want_id.trim();
        if id_ok(want_id) {
            let mut hits = candidates.iter().copied().filter(|&i| id_of(i).is_some_and(|id| id.trim() == want_id));
            if let (Some(i), None) = (hits.next(), hits.next()) {
                return Choice::Opens(i);
            }
        }
        return match match_endpoint(names, want) {
            Some(index) => Choice::Unsure { index, twins: candidates.len() },
            None => Choice::Twins(candidates.len()),
        };
    }
    if let Some(i) = match_endpoint(names, want) {
        return Choice::Opens(i);
    }
    if names.iter().any(|n| near_name(strip_index(n), stripped)) {
        return Choice::CaseOnly;
    }
    Choice::Missing
}

/// `want` names `endpoint` only inexactly: the same name in another case, or (a short or legacy
/// saved name, gate 2026-11-10) a whole bracketed part of it in any case: "Steam Streaming
/// Speakers" for "Speakers (Steam Streaming Speakers)". Never opens anything: the track waits
/// and asks for a fresh pick (A4: a track never records a sibling under its name). The
/// recorder keeps a copy (`recall` `audio/endpoints.rs` `near_name`); keep the two equal.
fn near_name(endpoint: &str, want: &str) -> bool {
    let want = want.trim().to_lowercase();
    if endpoint.trim().to_lowercase() == want {
        return true;
    }
    endpoint.split('(').skip(1).filter_map(|part| part.split_once(')')).any(|(inner, _)| inner.trim().to_lowercase() == want)
}

/// The endpoint the recorder records for `want`: its own, or for an `input` an unsure sibling
/// (muted). An output's unsure sibling records nothing (it waits).
pub fn opens(choice: Choice, input: bool) -> Option<usize> {
    match choice {
        Choice::Opens(i) => Some(i),
        Choice::Unsure { index, .. } if input => Some(index),
        _ => None,
    }
}

/// The strip note of an input on an unsure sibling (the recorder's status says the same).
pub fn unsure_note(name: &str) -> String {
    format!("{} may be a different device (its ID changed). Started muted; press Alt+M to unmute.", name.trim())
}

/// [`unsure_note`] for a line-in, which Alt+M never unmutes onto a sibling (P1-b; the
/// recorder's `endpoints::line_in_pick_notice`, same words).
pub fn unsure_line_in_note(name: &str) -> String {
    format!("{} may be a different device (its ID changed). Line-in stays muted: pick it on the Audio page.", name.trim())
}

/// The strip note of an output whose name two or more endpoints share (it waits).
pub fn twins_note(name: &str, twins: usize) -> String {
    format!("{}: {twins} devices share this name; waiting for the original", name.trim())
}

/// The strip note of a name that matches an endpoint only in another case.
pub fn case_note(name: &str) -> String {
    format!("{} not found exactly; pick it again", name.trim())
}

#[cfg(test)]
#[path = "endpoint_choice_tests.rs"]
mod tests;
