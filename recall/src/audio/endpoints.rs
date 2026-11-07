//! Which endpoint a device source opens, from the connected endpoints' friendly names. The
//! Audio page's meter (`recall-native` `services/meters.rs`) keeps a copy of `match_endpoint`,
//! so it meters the endpoint this records; keep the two equal (same test table in both).

/// `name` without a leading "<digits>- " Windows endpoint index: "4- Line (Box One)" → "Line
/// (Box One)". Nothing else is dropped or folded: an index inside a bracket, the case and every
/// other character stay.
fn strip_index(name: &str) -> &str {
    let name = name.trim();
    let digits = name.len() - name.trim_start_matches(|d: char| d.is_ascii_digit()).len();
    match name[digits..].strip_prefix("- ") {
        Some(rest) if digits > 0 => rest.trim_start(),
        _ => name,
    }
}

/// Index of the endpoint `want` names (2026-11-07 merge-gate rule): the one endpoint with
/// exactly that friendly name; else the one endpoint whose name equals `want`'s once both drop a
/// leading "<digits>- " index (the same endpoint renumbered by Windows). Case included, no word
/// or fuzzy matching. Two or more exact matches (identical devices), or two or more such
/// endpoints, match none, so a track never records a sibling endpoint under its name; it waits
/// for its own device.
pub(crate) fn match_endpoint(names: &[String], want: &str) -> Option<usize> {
    let want = want.trim();
    if want.is_empty() {
        return None;
    }
    let mut exact = names.iter().enumerate().filter(|(_, n)| n.trim() == want).map(|(i, _)| i);
    if let Some(first) = exact.next() {
        return exact.next().is_none().then_some(first);
    }
    let want = strip_index(want);
    let mut hits = names.iter().enumerate().filter(|(_, n)| strip_index(n) == want).map(|(i, _)| i);
    let first = hits.next()?;
    hits.next().is_none().then_some(first)
}

/// Index of the endpoint `want` names when it was saved with the endpoint id `want_id`: the
/// endpoint-id tie-break (A4 amendment, merge gate 2026-11-07 08:29 UTC) on top of
/// `match_endpoint`. When two or more endpoints have `want`'s name once a leading "<digits>- "
/// index is dropped (exact twins included), the one whose id (`id_of`, by index of `names`) is
/// the saved id opens. An id that matches none or several of them decides nothing: the rule
/// without ids (one exact name, else wait). Only same-named endpoints are candidates, so an id
/// never opens an endpoint by another name; ids are read only for the tie-break, and only with
/// a well-formed saved id. The Audio page's meter keeps a copy (`recall-native`
/// `services/meters.rs` `pick_endpoint`); keep the two equal (same test table in both).
pub(crate) fn pick_endpoint(names: &[String], want: &str, want_id: &str, id_of: impl Fn(usize) -> Option<String>) -> Option<usize> {
    let want_id = want_id.trim();
    if !want.trim().is_empty() && id_ok(want_id) {
        let stripped = strip_index(want);
        let candidates: Vec<usize> = names.iter().enumerate().filter(|(_, n)| strip_index(n) == stripped).map(|(i, _)| i).collect();
        if candidates.len() >= 2 {
            let mut hits = candidates.into_iter().filter(|&i| id_of(i).is_some_and(|id| id.trim() == want_id));
            if let (Some(i), None) = (hits.next(), hits.next()) {
                return Some(i);
            }
        }
    }
    match_endpoint(names, want)
}

/// A saved endpoint id the recorder trusts (R6-deep-audio-5): 1 to 256 characters once trimmed,
/// no control character. Anything else counts as no id: the source still records, and follows
/// Alt+M. The Audio page keeps a copy (`recall-native` `services/meters.rs` `id_ok`).
pub(crate) fn id_ok(id: &str) -> bool {
    let id = id.trim();
    !id.is_empty() && id.chars().count() <= 256 && !id.chars().any(char::is_control)
}

/// The endpoint a named source (`want`) opened is its own device (R6-deep-audio-1/2): the source
/// saved an endpoint id at pick time (`want_id`, well formed) and the opened endpoint has that
/// id. The id alone decides: a renumbered own device ("3- X" saved, "4- X" opened, same id) is
/// own; any name match with another id, or with no saved id, is not, so a line-in there
/// follows Alt+M (Windows can give the saved name to an identical dongle that is a microphone).
/// The Audio page keeps a copy (`recall-native` `services/meters.rs` `opened_own`); keep the two
/// equal (same test table in both).
pub(crate) fn opened_own(opened_id: Option<&str>, want: &str, want_id: &str) -> bool {
    let want_id = want_id.trim();
    !want.trim().is_empty() && id_ok(want_id) && opened_id.map(str::trim) == Some(want_id)
}

/// Which endpoint a device source opens.
#[derive(Debug, PartialEq)]
pub(crate) enum Pick {
    /// The connected endpoint at this index of the names.
    Named(usize),
    Default,
}

/// [`choose_endpoint_by`] with no saved endpoint id (tests).
#[cfg(test)]
pub(crate) fn choose_endpoint(names: &[String], want: Option<&str>, exact: bool) -> Result<Pick, String> {
    choose_endpoint_by(names, (want, ""), exact, |_| None)
}

/// The endpoint a device source asks for, from the connected endpoints' `names`: its
/// device `want` with the endpoint id saved with it (`pick_endpoint`, `id_of` reads an
/// endpoint's id by index), else (no device) the Windows default. A named device opens
/// by its own name only, at start and on every reopen, never as the Windows default: that would
/// record another device under this track's name (the track waits for it instead,
/// `capture_loop`). Under `RECALL_AUDIO_EXACT` (tests) only a named endpoint may open, so a test
/// never records the person's real devices.
pub(crate) fn choose_endpoint_by(names: &[String], (want, want_id): (Option<&str>, &str), exact: bool, id_of: impl Fn(usize) -> Option<String>) -> Result<Pick, String> {
    let want = want.map(str::trim).filter(|w| !w.is_empty());
    if let Some(i) = want.and_then(|w| pick_endpoint(names, w, want_id, &id_of)) {
        return Ok(Pick::Named(i));
    }
    if exact {
        return Err(format!("no endpoint matches {:?} (RECALL_AUDIO_EXACT)", want.map(str::to_owned)));
    }
    match want {
        Some(want) => Err(format!("'{want}' is not connected")),
        None => Ok(Pick::Default),
    }
}

#[cfg(test)]
#[path = "id_pick_tests.rs"]
mod id_pick_tests;

#[cfg(test)]
mod tests {
    use super::strip_index;

    #[test]
    fn only_a_leading_windows_index_is_dropped() {
        assert_eq!(strip_index("12- Speakers (Box One)"), "Speakers (Box One)");
        assert_eq!(strip_index("Microphone (2- Box One)"), "Microphone (2- Box One)", "an index in a bracket stays");
        assert_eq!(strip_index("Mic 2 (Box)"), "Mic 2 (Box)", "a number that is part of the name stays");
        assert_eq!(strip_index("2-3 Line"), "2-3 Line", "no space after the dash: not an index");
    }
}
