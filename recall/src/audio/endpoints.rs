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

/// What a named source's saved device (`want`, saved with the endpoint id `want_id`) picks
/// among the connected endpoints `names` (`id_of` reads an endpoint's id by index).
#[derive(Debug, PartialEq)]
pub(crate) enum Choice {
    /// The saved device: the one endpoint by its name, or among two or more endpoints with its
    /// name once a leading "<digits>- " index is dropped (exact twins included), the one with the
    /// saved id (A4 amendment, merge gate 2026-11-07 08:29 UTC).
    Opens(usize),
    /// A4 ruling (merge gate p9): two or more endpoints share the name, no saved id picks one,
    /// and this one has exactly the saved name. It may be another device whose id changed: an
    /// input opens it muted until a fresh Alt+M; an output waits (`choose_endpoint_by`).
    Unsure { index: usize, twins: usize },
    /// Two or more endpoints share the name and none has exactly the saved name: wait.
    Twins(usize),
    /// No endpoint has the name, but one has it in another case: wait, and say so (P2-d).
    CaseOnly,
    /// No endpoint has the name: wait.
    Missing,
}

/// The pick of `want` (see [`Choice`]). Case included, no word or fuzzy matching. Only
/// same-named endpoints are candidates, so an id never opens an endpoint by another name; ids
/// are read only for the tie-break, and only with a well-formed saved id.
pub(crate) fn choose(names: &[String], want: &str, want_id: &str, id_of: impl Fn(usize) -> Option<String>) -> Choice {
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
    let folded = stripped.to_lowercase();
    if names.iter().any(|n| strip_index(n).to_lowercase() == folded) {
        return Choice::CaseOnly;
    }
    Choice::Missing
}

/// Index of the endpoint `want` opens ([`choose`]): its own, or an unsure exact-name sibling.
/// The Audio page's meter keeps a copy (`recall-native` `services/meters.rs` `pick_endpoint`);
/// keep the two equal (same test table in both).
#[cfg(test)]
pub(crate) fn pick_endpoint(names: &[String], want: &str, want_id: &str, id_of: impl Fn(usize) -> Option<String>) -> Option<usize> {
    match choose(names, want, want_id, id_of) {
        Choice::Opens(i) | Choice::Unsure { index: i, .. } => Some(i),
        _ => None,
    }
}

/// The status of a track whose name two or more endpoints share and none is picked (A4 ruling).
pub(crate) fn twins_notice(name: &str, twins: usize) -> String {
    format!("{}: {twins} devices share this name; waiting for the original", name.trim())
}

/// The status of an input opened on an unsure exact-name sibling (A4 ruling).
pub(crate) fn unsure_notice(name: &str) -> String {
    format!("{} may be a different device (its ID changed). Started muted; press Alt+M to unmute.", name.trim())
}

/// The status of a line-in held on an unsure sibling after an unmuting Alt+M (P1-b): no id is
/// adopted for a line-in, so the user picks the device on the Audio page.
pub(crate) fn line_in_pick_notice(name: &str) -> String {
    format!("{} may be a different device (its ID changed). Line-in stays muted: pick it on the Audio page.", name.trim())
}

/// The status of a track whose name matches an endpoint only in another case (P2-d).
pub(crate) fn case_notice(name: &str) -> String {
    format!("{} not found exactly; pick it again", name.trim())
}

/// A pick failure the track shows as its status as it is (`capture_loop`), not as "not connected".
pub(crate) fn is_notice(why: &str) -> bool {
    why.ends_with("waiting for the original") || why.ends_with("pick it again")
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
    /// An input's unsure exact-name sibling (A4 ruling): it opens muted until a fresh Alt+M.
    Unsure(usize),
    Default,
}

/// [`choose_endpoint_by`] with no saved endpoint id (tests).
#[cfg(test)]
pub(crate) fn choose_endpoint(names: &[String], want: Option<&str>, exact: bool) -> Result<Pick, String> {
    choose_endpoint_by(names, (want, ""), (exact, true), |_| None)
}

/// The endpoint a device source asks for, from the connected endpoints' `names`: its
/// device `want` with the endpoint id saved with it ([`choose`], `id_of` reads an endpoint's id
/// by index), else (no device) the Windows default. A named device opens by its own name only,
/// at start and on every reopen, never as the Windows default: that would record another device
/// under this track's name (the track waits for it instead, `capture_loop`). An unsure sibling
/// opens only for an `input` (muted, `Pick::Unsure`); an output waits for the original. Under
/// `RECALL_AUDIO_EXACT` (`exact`, tests) only a named endpoint may open, so a test never records
/// the person's real devices.
pub(crate) fn choose_endpoint_by(names: &[String], (want, want_id): (Option<&str>, &str), (exact, input): (bool, bool), id_of: impl Fn(usize) -> Option<String>) -> Result<Pick, String> {
    let want = want.map(str::trim).filter(|w| !w.is_empty());
    if let Some(w) = want {
        match choose(names, w, want_id, &id_of) {
            Choice::Opens(i) => return Ok(Pick::Named(i)),
            Choice::Unsure { index, .. } if input => return Ok(Pick::Unsure(index)),
            Choice::Unsure { twins, .. } | Choice::Twins(twins) => return Err(twins_notice(w, twins)),
            Choice::CaseOnly => return Err(case_notice(w)),
            Choice::Missing => {}
        }
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
#[path = "unsure_pick_tests.rs"]
mod unsure_pick_tests;

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
