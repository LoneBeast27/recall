//! How a text run splits into the island's square cells (Rohan, 2026-11-06:
//! "assume all fields to be a group of squares (for rotational symmetry) and
//! all rectangles get divided to squares, though depending on aesthetics we
//! may fit 1 letter in the square or the full element").
//!
//! - A timer is one cell per part, minutes and seconds always 2 digits
//!   ("0:08" → "00" | "08", "12:34" → "12" | "34"); with hours the first
//!   cell is the hours as they are ("1:02:03" → "1" | "02" | "03").
//! - A number of up to 3 digits is one cell ("144", "98"); a longer one
//!   splits in groups of 3 from the right, the remainder first ("1440" → "1" | "440").
//! - A word of up to 3 glyphs is one cell ("REC", "64°"); a longer one
//!   splits into balanced pieces of at most 3, the longer pieces first
//!   ("SAVED" → "SAV" | "ED"). The layout asks for more pieces when one is
//!   too wide for its cell in the real font ([`pieces`]).
//! - "·" between words is no cell: the layout draws a dot in the gap.
//! - A trailing "…" is a cell of its own ("0:30…" → "00" | "30" | "…").

/// The most glyphs in one cell.
pub const MAX_CELL: usize = 3;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Token {
    /// A timer's parts, in order (a colon goes between them).
    Timer(Vec<String>),
    /// A run of digits, as its cells.
    Number(Vec<String>),
    /// A word (split into cells by the layout, which knows the widths).
    Word(String),
    /// "·": a dot in the gap.
    Dot,
    /// "…": a closing ellipsis cell.
    Ellipsis,
}

/// `run`'s tokens in reading order.
pub fn tokens(run: &str) -> Vec<Token> {
    let mut out = Vec::new();
    for word in run.split_whitespace() {
        if word == "·" {
            out.push(Token::Dot);
            continue;
        }
        let (body, ellipsis) = match word.strip_suffix('…') {
            Some(body) => (body, true),
            None => (word, false),
        };
        if let Some(parts) = timer(body) {
            out.push(Token::Timer(parts));
        } else if !body.is_empty() && body.bytes().all(|b| b.is_ascii_digit()) {
            out.push(Token::Number(digits(&body.chars().collect::<Vec<_>>())));
        } else if !body.is_empty() {
            out.push(Token::Word(body.to_string()));
        }
        if ellipsis {
            out.push(Token::Ellipsis);
        }
    }
    out
}

/// `h:mm:ss`, `m:ss` or `mm:ss` as its cells ("0:08" → "00", "08").
pub fn timer(word: &str) -> Option<Vec<String>> {
    let parts: Vec<&str> = word.split(':').collect();
    let ok = (2..=3).contains(&parts.len())
        && parts.iter().all(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()))
        && parts[1..].iter().all(|p| p.len() == 2)
        && parts[0].len() <= 2;
    if !ok {
        return None;
    }
    let first = if parts.len() == 3 { parts[0].to_string() } else { format!("{:0>2}", parts[0]) };
    Some(std::iter::once(first).chain(parts[1..].iter().map(|p| p.to_string())).collect())
}

/// Groups of [`MAX_CELL`] digits from the right, the remainder first.
fn digits(chars: &[char]) -> Vec<String> {
    let first = match chars.len() % MAX_CELL {
        0 => MAX_CELL.min(chars.len()),
        r => r,
    };
    let mut cells = vec![chars[..first].iter().collect::<String>()];
    cells.extend(chars[first..].chunks(MAX_CELL).map(|c| c.iter().collect::<String>()));
    cells.retain(|c| !c.is_empty());
    cells
}

/// `word` in the fewest balanced pieces of at most [`MAX_CELL`] glyphs that
/// all `fit` (one glyph a piece if nothing else does).
pub fn pieces(word: &str, fit: impl Fn(&str) -> bool) -> Vec<String> {
    let chars: Vec<char> = word.chars().collect();
    let mut n = chars.len().div_ceil(MAX_CELL).max(1);
    loop {
        let split = balanced(&chars, n);
        if n >= chars.len() || split.iter().all(|p| fit(p)) {
            return split;
        }
        n += 1;
    }
}

/// `chars` in `n` pieces, the longer pieces first.
fn balanced(chars: &[char], n: usize) -> Vec<String> {
    let (base, extra) = (chars.len() / n, chars.len() % n);
    let mut at = 0;
    (0..n)
        .map(|i| {
            let len = base + usize::from(i < extra);
            let piece = chars[at..at + len].iter().collect();
            at += len;
            piece
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| x.to_string()).collect()
    }

    #[test]
    fn timers_are_two_digit_minutes_and_seconds_hours_as_they_are() {
        assert_eq!(timer("0:08"), Some(s(&["00", "08"])));
        assert_eq!(timer("1:23"), Some(s(&["01", "23"])));
        assert_eq!(timer("12:34"), Some(s(&["12", "34"])));
        assert_eq!(timer("1:02:03"), Some(s(&["1", "02", "03"])));
        assert!(timer("1:2").is_none() && timer("123:45").is_none() && timer("a:bc").is_none());
    }

    #[test]
    fn numbers_stay_whole_to_three_digits_then_split_in_threes_from_the_right() {
        let number = |run: &str| match &tokens(run)[..] {
            [Token::Number(cells)] => cells.clone(),
            other => panic!("{other:?}"),
        };
        assert_eq!(number("144"), s(&["144"]));
        assert_eq!(number("7"), s(&["7"]));
        assert_eq!(number("1440"), s(&["1", "440"]));
        assert_eq!(number("1234567"), s(&["1", "234", "567"]));
    }

    #[test]
    fn a_detail_run_becomes_words_dots_timers_and_an_ellipsis() {
        assert_eq!(
            tokens("VALORANT · 0:30…"),
            [Token::Word("VALORANT".into()), Token::Dot, Token::Timer(s(&["00", "30"])), Token::Ellipsis]
        );
        assert_eq!(tokens("1% low 98"), [Token::Word("1%".into()), Token::Word("low".into()), Token::Number(s(&["98"]))]);
    }

    #[test]
    fn words_split_balanced_in_threes_and_more_when_too_wide() {
        assert_eq!(pieces("REC", |_| true), s(&["REC"]));
        assert_eq!(pieces("SAVED", |_| true), s(&["SAV", "ED"]));
        assert_eq!(pieces("Recording", |_| true), s(&["Rec", "ord", "ing"]));
        assert_eq!(pieces("WWW", |p| p.chars().count() <= 2), s(&["WW", "W"]));
        assert_eq!(pieces("WW", |_| false), s(&["W", "W"]), "one glyph a cell at worst");
    }
}
