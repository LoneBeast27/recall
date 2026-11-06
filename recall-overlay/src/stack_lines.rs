//! How a side spot's upright column groups a text run into lines (Rohan,
//! 2026-11-06: "would be ideal if we have numbers xx or group in letters of
//! 3 or something; x/x/x is too separated").
//!
//! - Words: a word of 1-3 letters is one line; a longer one splits into
//!   balanced lines of at most 3 ("SAVED" → "SAV" / "ED", "Clip" → "Cl" / "ip").
//! - Numbers: at most 2 digits per line, the remainder first ("144" → "1" / "44").
//! - A timer drops its colons: one line per part ("1:23" → "1" / "23",
//!   "1:02:03" → "1" / "02" / "03").
//! - A number with its unit stays one line up to 3 glyphs ("1%", "64°"); a
//!   longer mix keeps its first unit glyph with a short number ("64°C" →
//!   "64°" / "C").
//!
//! Each word is a list of lines; words are separated by a small gap, lines
//! within a word by the tight line pitch.

/// The most glyphs on one line.
pub const MAX_LINE: usize = 3;

/// `run`'s words, each as its upright lines.
pub fn group(run: &str) -> Vec<Vec<String>> {
    run.split_whitespace().map(word_lines).filter(|w| !w.is_empty()).collect()
}

fn word_lines(word: &str) -> Vec<String> {
    let chars: Vec<char> = word.chars().collect();
    if is_timer(word) {
        return word.split(':').map(str::to_string).collect();
    }
    if chars.iter().all(char::is_ascii_digit) {
        return digits(&chars);
    }
    if chars.len() <= MAX_LINE {
        return vec![word.to_string()];
    }
    // A number with a unit: the number's digits, then the unit as a word.
    let lead = chars.iter().take_while(|c| c.is_ascii_digit()).count();
    if (1..chars.len()).contains(&lead) && chars.len() - lead <= MAX_LINE {
        // A short number keeps its first unit glyph ("64°" / "C"): a lone "°"
        // or "%" line would float above the cap height and crowd the next one.
        if lead < MAX_LINE {
            let rest: String = chars[lead + 1..].iter().collect();
            let mut lines = vec![chars[..=lead].iter().collect::<String>()];
            lines.extend((!rest.is_empty()).then_some(rest));
            return lines;
        }
        let mut lines = digits(&chars[..lead]);
        lines.push(chars[lead..].iter().collect());
        return lines;
    }
    balanced(&chars)
}

/// `h:mm:ss`, `m:ss` or `mm:ss`.
fn is_timer(word: &str) -> bool {
    let parts: Vec<&str> = word.split(':').collect();
    (2..=3).contains(&parts.len())
        && parts.iter().all(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()))
        && parts[1..].iter().all(|p| p.len() == 2)
        && parts[0].len() <= 2
}

/// Pairs of digits, the odd one out first ("144" → "1", "44").
fn digits(chars: &[char]) -> Vec<String> {
    let first = match chars.len() % 2 {
        0 => 2.min(chars.len()),
        _ => 1,
    };
    let mut lines = vec![chars[..first].iter().collect::<String>()];
    lines.extend(chars[first..].chunks(2).map(|c| c.iter().collect::<String>()));
    lines.retain(|l| !l.is_empty());
    lines
}

/// The fewest lines of at most [`MAX_LINE`], the longer lines first.
fn balanced(chars: &[char]) -> Vec<String> {
    let n = chars.len().div_ceil(MAX_LINE);
    let (base, extra) = (chars.len() / n, chars.len() % n);
    let mut at = 0;
    (0..n)
        .map(|i| {
            let len = base + usize::from(i < extra);
            let line = chars[at..at + len].iter().collect();
            at += len;
            line
        })
        .collect()
}

/// Whether a line hangs below the baseline (it needs a little more room under it).
pub fn descends(line: &str) -> bool {
    line.chars().any(|c| "gjpqy,;".contains(c))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lines(run: &str) -> Vec<Vec<&'static str>> {
        group(run).into_iter().map(|w| w.into_iter().map(|l| &*Box::leak(l.into_boxed_str())).collect()).collect()
    }

    #[test]
    fn words_group_in_threes_and_short_words_stay_whole() {
        assert_eq!(lines("REC"), [vec!["REC"]]);
        assert_eq!(lines("SAVED"), [vec!["SAV", "ED"]]);
        assert_eq!(lines("Clip saved"), [vec!["Cl", "ip"], vec!["sav", "ed"]]);
        assert_eq!(lines("1% low 98"), [vec!["1%"], vec!["low"], vec!["98"]]);
        assert_eq!(lines("Recording"), [vec!["Rec", "ord", "ing"]]);
        assert_eq!(lines("Hidden"), [vec!["Hid", "den"]]);
    }

    #[test]
    fn numbers_take_two_digits_a_line_the_remainder_first() {
        assert_eq!(lines("144"), [vec!["1", "44"]]);
        assert_eq!(lines("98"), [vec!["98"]]);
        assert_eq!(lines("1440"), [vec!["14", "40"]]);
        assert_eq!(lines("12345"), [vec!["1", "23", "45"]]);
    }

    #[test]
    fn timers_drop_the_colon_one_line_per_part() {
        assert_eq!(lines("1:23"), [vec!["1", "23"]]);
        assert_eq!(lines("12:34"), [vec!["12", "34"]]);
        assert_eq!(lines("2:00"), [vec!["2", "00"]]);
        assert_eq!(lines("1:02:03"), [vec!["1", "02", "03"]]);
        assert!(!is_timer("1:2") && !is_timer("123:45") && !is_timer("a:bc"));
    }

    #[test]
    fn a_number_keeps_its_unit_up_to_three_glyphs() {
        assert_eq!(lines("64°"), [vec!["64°"]]);
        assert_eq!(lines("1%"), [vec!["1%"]]);
        assert_eq!(lines("64°C"), [vec!["64°", "C"]]);
        assert_eq!(lines("120fps"), [vec!["1", "20", "fps"]]);
    }

    #[test]
    fn no_line_is_longer_than_three_glyphs() {
        for run in ["Island hidden by fullscreen", "Rocket League runs in exclusive fullscreen.", "VALORANT · 2:00", "2:001441%low98GPU64°CCPU"] {
            for line in group(run).iter().flatten() {
                assert!((1..=MAX_LINE).contains(&line.chars().count()), "{run}: {line:?}");
            }
        }
    }
}
