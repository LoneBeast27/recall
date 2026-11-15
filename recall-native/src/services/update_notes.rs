//! The release notes as lines for the update card (headings, bullets, text).

#[derive(Debug, Clone, PartialEq)]
pub enum NoteLine {
    Heading(String),
    Bullet(String),
    Text(String),
}

pub fn note_lines(notes: &str) -> Vec<NoteLine> {
    notes
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .take(100)
        .map(|line| {
            let clean = |s: &str| s.replace("**", "").replace('`', "");
            if let Some(h) = line.strip_prefix('#') {
                NoteLine::Heading(clean(h.trim_start_matches('#').trim()))
            } else if let Some(b) = line.strip_prefix("- ").or_else(|| line.strip_prefix("* ")) {
                NoteLine::Bullet(clean(b.trim()))
            } else {
                NoteLine::Text(clean(line))
            }
        })
        .collect()
}
