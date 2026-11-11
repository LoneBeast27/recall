//! A content check for trace and crash text: finds anything that looks like a path, a user
//! name, an email address or a file name. Tests and the release check run it over every
//! trace they produce; the trace API should make a hit impossible.

/// The first piece of `text` that looks like content, if any. `user` is the Windows user
/// name to look for (none: only the shape checks run).
pub fn find_content(text: &str, user: Option<&str>) -> Option<String> {
    let lower = text.to_ascii_lowercase();
    let markers = [":\\", ":/", "\\\\", "/users/", "/home/", "appdata", ".exe", ".mp4", ".mkv", ".toml", ".json"];
    for marker in markers {
        if let Some(i) = lower.find(marker) {
            return Some(snippet(text, i));
        }
    }
    if let Some(i) = email_at(text.as_bytes()) {
        return Some(snippet(text, i));
    }
    if let Some(user) = user.filter(|u| u.len() >= 3) {
        if let Some(i) = lower.find(&user.to_ascii_lowercase()) {
            return Some(snippet(text, i));
        }
    }
    None
}

fn is_mail_char(c: u8) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, b'.' | b'_' | b'-' | b'+')
}

/// The index of an `@` with a name before it and a dotted domain after it.
fn email_at(bytes: &[u8]) -> Option<usize> {
    (1..bytes.len()).find(|&i| {
        bytes[i] == b'@'
            && is_mail_char(bytes[i - 1])
            && bytes[i + 1..].iter().take_while(|c| is_mail_char(**c)).any(|c| *c == b'.')
    })
}

fn snippet(text: &str, at: usize) -> String {
    let start = text[..at].char_indices().rev().nth(12).map_or(0, |(i, _)| i);
    text[start..].chars().take(32).collect()
}
