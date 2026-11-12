//! Anonymous copies of Recall's developer logs and settings for a report. An allow list, not a
//! deny list: a word is kept only when Recall's own code contains it (the vocabulary
//! `build.rs` collects from the source's string literals), so names of games, clips, devices,
//! monitors, folders and people are replaced by `<w>` wherever they appear. On top of that:
//! paths become `<path>` (to the closing quote, or to the line end when unquoted), email
//! addresses `<email>`, the Windows user and PC names `<user>` and `<pc>`, numbers mixed with
//! letters (ids, GUIDs) `<n>`. Plain numbers, short hex codes and punctuation stay, so the
//! shape of every line (states, counts, timings, error codes) survives.

use std::collections::HashSet;
use std::sync::OnceLock;

/// The names to hide, read from the environment at report time.
#[derive(Clone, Debug, Default)]
pub struct Names {
    pub user: Option<String>,
    pub pc: Option<String>,
}

impl Names {
    pub fn from_env() -> Self {
        let get = |k: &str| std::env::var(k).ok().filter(|v| !v.trim().is_empty());
        Names { user: get("USERNAME"), pc: get("COMPUTERNAME") }
    }
}

fn vocab() -> &'static HashSet<&'static str> {
    static VOCAB: OnceLock<HashSet<&'static str>> = OnceLock::new();
    VOCAB.get_or_init(|| include_str!(concat!(env!("OUT_DIR"), "/vocab.txt")).lines().collect())
}

/// Whether `word` (lower case) is one of Recall's own words.
pub fn known_word(word: &str) -> bool {
    vocab().contains(word)
}

/// One redacted copy of `text`.
pub fn redact(text: &str, names: &Names) -> String {
    let mut out = String::with_capacity(text.len());
    let mut section = String::new();
    for line in text.split_inclusive('\n') {
        let trimmed = line.trim();
        if trimmed.starts_with('[') && trimmed.ends_with(']') && !trimmed.contains(' ') {
            section = trimmed.trim_matches(['[', ']']).to_string();
        }
        // Settings tables keyed by game (the folder each game's clips go to): every key is a
        // game name, whatever its words.
        if NAME_KEYED_TABLES.contains(&section.as_str()) && !trimmed.starts_with('[') {
            if let Some((_, value)) = line.split_once('=') {
                out.push_str(&redact_line(&format!("<app> ={value}"), names));
                continue;
            }
        }
        out.push_str(&redact_line(line, names));
    }
    out
}

/// Settings tables whose keys are game names.
const NAME_KEYED_TABLES: &[&str] = &["recording.game_folder_choices"];

/// Media file extensions: a name ending in one is a clip or screenshot name.
const FILE_EXTS: &[&str] = &[".mp4", ".mkv", ".mov", ".webm", ".avi", ".png", ".jpg", ".jpeg", ".gif", ".wav", ".flac", ".m4a", ".mp3"];

/// A file name ending in a media extension, back to the delimiter before it (a clip name may
/// hold spaces and ordinary words: "Funny Moment 2026-11-12.mp4" becomes "<file>.mp4").
fn redact_file_names(line: &str) -> String {
    let lower = line.to_ascii_lowercase();
    let mut cuts: Vec<(usize, usize, &str)> = Vec::new();
    for ext in FILE_EXTS {
        for (at, _) in lower.match_indices(ext) {
            let end = at + ext.len();
            if lower[end..].starts_with(|c: char| c.is_ascii_alphanumeric()) {
                continue; // ".mp4x" or ".pngs": not an extension
            }
            // Back to the delimiter, passing over JSON's escaped quotes (`\"`).
            let mut start = 0;
            let mut scan = at;
            while let Some(d) = line[..scan].rfind([':', '"', '\'', '(', '[', '{', '=', ',', '\\', '/', '\t']) {
                let escaped_quote = line.as_bytes()[d] == b'"' && d > 0 && line.as_bytes()[d - 1] == b'\\';
                let escape = line.as_bytes()[d] == b'\\' && line.as_bytes().get(d + 1) == Some(&b'"');
                if escaped_quote || escape {
                    scan = d;
                    continue;
                }
                start = d + 1;
                break;
            }
            let start = start + line[start..at].len() - line[start..at].trim_start().len();
            if start < at && !cuts.iter().any(|(s, e, _)| start < *e && *s < end) {
                cuts.push((start, end, ext));
            }
        }
    }
    cuts.sort();
    let mut out = String::with_capacity(line.len());
    let mut last = 0;
    for (start, end, ext) in cuts {
        out.push_str(&line[last..start]);
        out.push_str("<file>");
        out.push_str(ext);
        last = end;
    }
    out.push_str(&line[last..]);
    out
}

fn redact_line(line: &str, names: &Names) -> String {
    let line = redact_file_names(line);
    let chars: Vec<char> = line.chars().collect();
    let mut out = String::with_capacity(line.len());
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        let prev = if i > 0 { Some(chars[i - 1]) } else { None };
        let drive = i + 2 < chars.len() && c.is_ascii_alphabetic() && chars[i + 1] == ':' && matches!(chars[i + 2], '\\' | '/') && !prev.is_some_and(|p| p.is_alphanumeric());
        let unc = c == '\\' && chars.get(i + 1) == Some(&'\\') && !prev.is_some_and(|p| p == '\\');
        // `%NAME%` (an environment variable); "45% load, CPU 30%" is not one.
        let env = c == '%'
            && chars[i + 1..].iter().take(24).position(|ch| *ch == '%').is_some_and(|p| {
                p > 0 && chars[i + 1].is_ascii_alphabetic() && chars[i + 1..i + 1 + p].iter().all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '_' | '(' | ')'))
            });
        if drive || unc || env {
            // Quoted: to the closing quote. Unquoted: to the line end ("Program Files (x86)").
            let quote = prev.filter(|p| matches!(p, '"' | '\''));
            while i < chars.len() && chars[i] != '\n' && chars[i] != '\r' && Some(chars[i]) != quote {
                i += 1;
            }
            out.push_str("<path>");
            continue;
        }
        if c.is_alphanumeric() {
            let start = i;
            while i < chars.len() && (chars[i].is_alphanumeric() || matches!(chars[i], '_' | '.' | '@' | '+' | '-') && chars.get(i + 1).is_some_and(|n| n.is_alphanumeric())) {
                i += 1;
            }
            let token: String = chars[start..i].iter().collect();
            out.push_str(&redact_token(&token, names));
            continue;
        }
        out.push(c);
        i += 1;
    }
    // Slots that always hold a program's name, whatever its words: ordinary words such as
    // "Client" are in Recall's vocabulary, the program they name is not.
    for (start, end) in [("foreground: ", " pid "), ("over ", " pid "), ("rule set: ", " -> "), ("placed: ", " anchor "), ("target -> \"", "\"")] {
        out = redact_between(&out, start, end, "<app>");
    }
    // A game notice's detail is the game's display name ("Recording Ready or Not").
    if let Some(at) = out.find("notice game: Recording ").map(|i| i + "notice game: Recording ".len()) {
        let end = out[at..].find(['\r', '\n']).map_or(out.len(), |e| at + e);
        out.replace_range(at..end, "<app>");
    }
    collapse(&whole_groups(&whole_quotes(&out)))
}

/// A bracketed group `( … )` with a replaced word in it is replaced whole, as quotes are
/// ("Microphone (2- Some Brand Chat)" becomes "Microphone (<w>)").
fn whole_groups(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut rest = line;
    while let Some(open) = rest.find('(') {
        let after = &rest[open + 1..];
        let Some(close) = after.find(')') else { break };
        let inner = &after[..close];
        out.push_str(&rest[..open + 1]);
        let nested = inner.contains('(');
        out.push_str(if !nested && (inner.contains("<w>") || inner.contains("<n>")) { "<w>" } else { inner });
        out.push(')');
        rest = &after[close + 1..];
    }
    out.push_str(rest);
    out
}

/// A quoted value with any replaced word in it is replaced whole: a device such as
/// "Speakers (Some Brand Chat)" keeps no ordinary words that could still point at it.
fn whole_quotes(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut rest = line;
    while let Some(open) = rest.find(['"', '\'']) {
        let quote = rest.as_bytes()[open] as char;
        let after = &rest[open + 1..];
        // An apostrophe inside a word ("recorder's") opens nothing.
        let in_word = quote == '\'' && open > 0 && rest.as_bytes()[open - 1].is_ascii_alphanumeric();
        let Some(close) = after.find(quote).filter(|_| !in_word) else {
            out.push_str(&rest[..open + 1]);
            rest = after;
            continue;
        };
        let inner = &after[..close];
        out.push_str(&rest[..open + 1]);
        out.push_str(if inner.contains("<w>") || inner.contains("<n>") { "<w>" } else { inner });
        out.push(quote);
        rest = &after[close + 1..];
    }
    out.push_str(rest);
    out
}

/// Replace what stands between `start` and the next `end` in `line` with `mark`.
fn redact_between(line: &str, start: &str, end: &str, mark: &str) -> String {
    let Some(at) = line.find(start).map(|i| i + start.len()) else { return line.to_string() };
    let Some(len) = line[at..].find(end) else { return line.to_string() };
    if line[at..at + len].trim() == "none" {
        return line.to_string();
    }
    format!("{}{mark}{}", &line[..at], &line[at + len..])
}

/// A run of letters, digits and inner `_ . @ + -` (`save.done`, `a.b@x.com`, `0x80070057`).
fn redact_token(token: &str, names: &Names) -> String {
    if token.contains('@') && token.split('@').nth(1).is_some_and(|d| d.contains('.')) {
        return "<email>".into();
    }
    let lower = token.to_lowercase();
    for (name, mark) in [(&names.user, "<user>"), (&names.pc, "<pc>")] {
        if name.as_deref().is_some_and(|n| n.to_lowercase() == lower) {
            return mark.into();
        }
    }
    // Pieces joined by `.`, `-`, `+` or `@` are judged one by one (`recall-hdr`, `0.1.5`).
    let mut out = String::with_capacity(token.len());
    let mut piece = String::new();
    for c in token.chars().chain(std::iter::once('\0')) {
        if c == '\0' || matches!(c, '.' | '-' | '+' | '@') {
            out.push_str(&judge(&piece, names));
            piece.clear();
            if c != '\0' {
                out.push(c);
            }
        } else {
            piece.push(c);
        }
    }
    out
}

fn judge(piece: &str, names: &Names) -> String {
    if piece.is_empty() {
        return String::new();
    }
    let lower = piece.to_lowercase();
    for (name, mark) in [(&names.user, "<user>"), (&names.pc, "<pc>")] {
        if name.as_deref().is_some_and(|n| n.to_lowercase() == lower) {
            return mark.into();
        }
    }
    if piece.chars().all(|c| c.is_ascii_digit()) || (piece.len() == 1 && piece.is_ascii()) {
        return piece.into();
    }
    let hex = lower.strip_prefix("0x").is_some_and(|h| !h.is_empty() && h.len() <= 16 && h.chars().all(|c| c.is_ascii_hexdigit()));
    if hex {
        return piece.into();
    }
    if piece.starts_with(|c: char| c.is_ascii_digit()) {
        return if unit_number(&lower) { piece.into() } else { "<n>".into() };
    }
    // Recall's own words, also as parts joined by `_` (EVENT_SYSTEM_FOREGROUND, notices_on).
    if piece.is_ascii() && (known_word(&lower) || lower.split('_').all(|w| w.is_empty() || known_word(w) || w.chars().all(|c| c.is_ascii_digit()))) {
        return piece.into();
    }
    "<w>".into()
}

/// A number with a unit (`812ms`, `60fps`, `48khz`, `1440p`) or a size (`2560x1440`).
fn unit_number(lower: &str) -> bool {
    const UNITS: &[&str] = &["ms", "s", "us", "ns", "fps", "hz", "khz", "kb", "mb", "gb", "tb", "kib", "mib", "gib", "p", "k", "m", "x", "ch", "bit", "bps", "kbps", "mbps"];
    let digits = lower.find(|c: char| !c.is_ascii_digit()).unwrap_or(lower.len());
    let (_, unit) = lower.split_at(digits);
    if let Some(rest) = unit.strip_prefix('x') {
        if !rest.is_empty() && rest.chars().all(|c| c.is_ascii_digit()) {
            return true;
        }
    }
    UNITS.contains(&unit)
}

/// `<w> <w> <w>` reads as one `<w>`.
fn collapse(line: &str) -> String {
    let mut out = line.to_string();
    for mark in ["<w>", "<n>"] {
        let double = format!("{mark} {mark}");
        while out.contains(&double) {
            out = out.replace(&double, mark);
        }
    }
    out
}
