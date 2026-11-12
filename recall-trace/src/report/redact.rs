//! Anonymous copies of Recall's developer logs and settings for a report: the Windows user
//! name, the PC name, folder paths, program names, clip names and email addresses are
//! replaced by placeholders (`<user>`, `<pc>`, `<path>`, `<app>.exe`, `<clip>`, `<email>`).
//! It errs on the side of removing too much: a path runs to the next quote, bracket, comma or
//! line end, because folder names may hold spaces.

/// The names to hide, read from the environment at report time.
#[derive(Clone, Debug, Default)]
pub struct Names {
    pub user: Option<String>,
    pub pc: Option<String>,
}

impl Names {
    pub fn from_env() -> Self {
        let get = |k: &str| std::env::var(k).ok().filter(|v| v.len() >= 2);
        Names { user: get("USERNAME"), pc: get("COMPUTERNAME") }
    }
}

/// One redacted copy of `text`.
pub fn redact(text: &str, names: &Names) -> String {
    let mut out = String::with_capacity(text.len());
    for line in text.split_inclusive('\n') {
        out.push_str(&redact_line(line));
    }
    for (name, mark) in [(&names.user, "<user>"), (&names.pc, "<pc>")] {
        if let Some(name) = name {
            out = replace_ci(&out, name, mark);
        }
    }
    redact_emails(&out)
}

fn ends_path(c: char) -> bool {
    matches!(c, '"' | '\'' | ')' | ']' | '}' | ',' | ';' | '\n' | '\r' | '<' | '>' | '|')
}

fn redact_line(line: &str) -> String {
    let chars: Vec<char> = line.chars().collect();
    let json = line.trim_start().starts_with('{');
    let mut out = String::with_capacity(line.len());
    let mut i = 0;
    while i < chars.len() {
        let drive = i + 2 < chars.len() && chars[i].is_ascii_alphabetic() && chars[i + 1] == ':' && matches!(chars[i + 2], '\\' | '/') && (i == 0 || !chars[i - 1].is_ascii_alphanumeric());
        let unc = i + 1 < chars.len() && chars[i] == '\\' && chars[i + 1] == '\\';
        let env = chars[i] == '%' && chars[i + 1..].iter().take(16).position(|c| *c == '%').is_some_and(|p| p > 0);
        if drive || unc || env {
            while i < chars.len() && !ends_path(chars[i]) {
                i += 1;
            }
            out.push_str("<path>");
            continue;
        }
        // A word ending in `.exe`: a program name.
        if chars[i].is_alphanumeric() && (i == 0 || !is_name_char(chars[i - 1])) {
            let start = i;
            while i < chars.len() && is_name_char(chars[i]) {
                i += 1;
            }
            let word: String = chars[start..i].iter().collect();
            if word.to_ascii_lowercase().ends_with(".exe") {
                out.push_str("<app>.exe");
            } else {
                out.push_str(&word);
            }
            continue;
        }
        // Quoted text in a plain log line (a title, a device): kept out whole.
        if !json && matches!(chars[i], '"' | '\'') && (i == 0 || !chars[i - 1].is_alphanumeric()) {
            let quote = chars[i];
            if let Some(len) = chars[i + 1..].iter().position(|c| *c == quote || *c == '\n') {
                if chars[i + 1 + len] == quote {
                    out.push(quote);
                    out.push_str("<text>");
                    out.push(quote);
                    i += len + 2;
                    continue;
                }
            }
        }
        out.push(chars[i]);
        i += 1;
    }
    if json {
        return redact_json_names(&out);
    }
    // Words the island and recorder logs put after fixed phrases: a window class names its
    // game, and the island's placement lines name the program they were made for.
    let mut out = redact_after(&out, "class ", "<class>");
    out = redact_after(&out, "placed: ", "<app>");
    if out.contains("editor: ") {
        out = redact_after(&out, " for ", "<app>");
    }
    out
}

/// Replace the word after every `marker` in `line` with `mark`.
fn redact_after(line: &str, marker: &str, mark: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut rest = line;
    while let Some(at) = rest.find(marker) {
        let start = at + marker.len();
        out.push_str(&rest[..start]);
        let tail = &rest[start..];
        let end = tail.find(|c: char| c.is_whitespace() || matches!(c, '(' | ')' | ',' | ';' | '[' | ']')).unwrap_or(tail.len());
        if end > 0 && !tail[..end].starts_with('<') {
            out.push_str(mark);
        } else {
            out.push_str(&tail[..end]);
        }
        rest = &tail[end..];
    }
    out.push_str(rest);
    out
}

fn is_name_char(c: char) -> bool {
    c.is_alphanumeric() || matches!(c, '.' | '_' | '-' | '+')
}

/// JSON fields that name a clip, a device, a window or a game keep their key, not their value.
fn redact_json_names(line: &str) -> String {
    const KEYS: &[&str] = &["clip", "name", "title", "device", "file", "path", "game", "window", "folder", "label"];
    let mut out = line.to_string();
    for key in KEYS {
        let needle = format!("\"{key}\":\"");
        let mut from = 0;
        while let Some(at) = out[from..].find(&needle).map(|p| p + from) {
            let value_at = at + needle.len();
            let Some(end) = out[value_at..].find('"').map(|p| p + value_at) else { break };
            out.replace_range(value_at..end, "<clip>");
            from = value_at + "<clip>".len() + 1;
        }
    }
    out
}

/// Replace `word` in any letter case (ASCII case folding, so byte positions line up).
fn replace_ci(text: &str, word: &str, mark: &str) -> String {
    if word.is_empty() {
        return text.to_string();
    }
    let lower = text.to_ascii_lowercase();
    let word = word.to_ascii_lowercase();
    let mut out = String::with_capacity(text.len());
    let mut last = 0;
    for (at, _) in lower.match_indices(&word) {
        out.push_str(&text[last..at]);
        out.push_str(mark);
        last = at + word.len();
    }
    out.push_str(&text[last..]);
    out
}

fn redact_emails(text: &str) -> String {
    let mail = |c: char| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-' | '+');
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    while i < chars.len() {
        if chars[i] == '@' && i > 0 && mail(chars[i - 1]) {
            let after: Vec<char> = chars[i + 1..].iter().copied().take_while(|c| mail(*c)).collect();
            if after.contains(&'.') {
                while out.chars().last().is_some_and(mail) {
                    out.pop();
                }
                out.push_str("<email>");
                i += 1 + after.len();
                continue;
            }
        }
        out.push(chars[i]);
        i += 1;
    }
    out
}
