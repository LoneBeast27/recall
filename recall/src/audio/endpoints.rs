//! Which endpoint a device source opens, from the connected endpoints' friendly names. The
//! Audio page's meter (`recall-native` `services/meters.rs`) keeps a copy of `match_endpoint`,
//! so it meters the endpoint this records; keep the two equal (same test table in both).

/// Lowercase, ø→o (a name typed without the ø still matches), split into words.
fn words(text: &str) -> Vec<String> {
    let folded: String = text.to_lowercase().chars().map(|c| if c == 'ø' { 'o' } else { c }).collect();
    folded.split(|c: char| !c.is_alphanumeric()).filter(|w| !w.is_empty()).map(str::to_owned).collect()
}

/// True when every word of `want` is a whole word of `name`: "a" never matches inside
/// another word.
fn name_matches(name: &str, want: &str) -> bool {
    let name = words(name);
    let want = words(want);
    !want.is_empty() && want.iter().all(|w| name.contains(w))
}

/// `name` without Windows' "N- " endpoint index at its start or at the start of a bracket:
/// "Microphone (2- USB Audio Device)" → "Microphone (USB Audio Device)".
fn strip_index(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    let mut rest = name.trim();
    let mut at_start = true;
    while let Some(c) = rest.chars().next() {
        if at_start {
            let digits = rest.len() - rest.trim_start_matches(|d: char| d.is_ascii_digit()).len();
            if digits > 0 && rest[digits..].starts_with('-') {
                rest = rest[digits + 1..].trim_start();
                at_start = false;
                continue;
            }
        }
        at_start = c == '(';
        out.push(c);
        rest = &rest[c.len_utf8()..];
    }
    out
}

/// Index of the endpoint `want` names: an exact (case-insensitive) friendly name first; else
/// the one endpoint whose words hold `want`'s once both drop their "N- " index (the same
/// model renumbered after a port change). Two or more such endpoints match none: two
/// "USB Audio Device"s can't be told apart, so the track waits for its own device instead of
/// recording a sibling under its name.
pub(crate) fn match_endpoint(names: &[String], want: &str) -> Option<usize> {
    let want = want.trim();
    if want.is_empty() {
        return None;
    }
    let lower = want.to_lowercase();
    if let Some(i) = names.iter().position(|n| n.trim().to_lowercase() == lower) {
        return Some(i);
    }
    let want = strip_index(want);
    let mut hits = names.iter().enumerate().filter(|(_, n)| name_matches(&strip_index(n), &want)).map(|(i, _)| i);
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
    fn the_windows_index_is_dropped_only_where_windows_puts_it() {
        assert_eq!(strip_index("Microphone (2- USB Audio Device)"), "Microphone (USB Audio Device)");
        assert_eq!(strip_index("12- Speakers"), "Speakers");
        assert_eq!(strip_index("Mic 2 (USB)"), "Mic 2 (USB)", "a number that is part of the name stays");
        assert_eq!(strip_index("Line (Mixér 2-3)"), "Line (Mixér 2-3)");
    }
}
