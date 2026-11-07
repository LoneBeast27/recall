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

/// Index of the endpoint `want` names (2026-11-07 merge-gate rule): the endpoint with exactly
/// that friendly name; else the one endpoint whose name equals `want`'s once both drop a leading
/// "<digits>- " index (the same endpoint renumbered by Windows). Case included, no word or fuzzy
/// matching. Two or more such endpoints match none, so a track never records a sibling endpoint
/// under its name; it waits for its own device.
pub(crate) fn match_endpoint(names: &[String], want: &str) -> Option<usize> {
    let want = want.trim();
    if want.is_empty() {
        return None;
    }
    if let Some(i) = names.iter().position(|n| n.trim() == want) {
        return Some(i);
    }
    let want = strip_index(want);
    let mut hits = names.iter().enumerate().filter(|(_, n)| strip_index(n) == want).map(|(i, _)| i);
    let first = hits.next()?;
    hits.next().is_none().then_some(first)
}

/// Which endpoint a device source opens.
#[derive(Debug, PartialEq)]
pub(crate) enum Pick {
    /// The connected endpoint at this index of the names.
    Named(usize),
    Default,
}

/// The endpoint a device source asks for, from the connected endpoints' `names`: its
/// device `want` (`match_endpoint`), else (no device) the Windows default. A named device opens
/// by its own name only, at start and on every reopen, never as the Windows default: that would
/// record another device under this track's name (the track waits for it instead,
/// `capture_loop`). Under `RECALL_AUDIO_EXACT` (tests) only a named endpoint may open, so a test
/// never records the person's real devices.
pub(crate) fn choose_endpoint(names: &[String], want: Option<&str>, exact: bool) -> Result<Pick, String> {
    let want = want.map(str::trim).filter(|w| !w.is_empty());
    if let Some(i) = want.and_then(|w| match_endpoint(names, w)) {
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
