//! Recorder file names → (game prefix, local start) and game display names.
//!
//! Shapes on the user's drive (4,365 clips, Oct 2026):
//! `GAME__2026-06-02__10-01-55.mp4` (almost all), `GAME__2026-10-17__22-34-31-275__<32 hex>.mp4`
//! (Recall), `GAME__…__13-06-02_trim.mp4`, `GAME__…__23-23-00 BLIND FUZE.mp4`,
//! `Valorant_2023_01_27_10_10_11.mp4` (older capture tool), `lethal company moment.mp4` (no date).
//! Imported folders add `Rocket League 2024.01.27 - 22.10.11.03.DVR.mp4` (ShadowPlay),
//! `Rocket League 2024-01-27 22-10-11.mp4` (Game Bar), `2024-01-27 22-10-11.mkv` (OBS).
use super::LocalStamp;
use super::calendar::valid_date;

/// (key after cleaning, merged key, display). Keys not listed keep their cleaned prefix.
const OVERRIDES: &[(&str, &str, &str)] = &[
    ("desktopcapture", "desktop", "Desktop"),
    ("desktop", "desktop", "Desktop"),
    ("counterstrike2", "counterstrike2", "Counter-Strike 2"),
    ("counterstrikeglobaloffensive", "counterstrikeglobaloffensive", "Counter-Strike: Global Offensive"),
    ("marvelsspidermanremastered", "marvelsspidermanremastered", "Marvel's Spider-Man Remastered"),
    ("tomclancysrainbowsixsiege", "tomclancysrainbowsixsiege", "Rainbow Six Siege"),
    ("rainbowsixsiege", "tomclancysrainbowsixsiege", "Rainbow Six Siege"),
    ("genshinimpact", "genshinimpact", "Genshin Impact"),
    ("rocketleague", "rocketleague", "Rocket League"),
    ("thefinals", "thefinals", "THE FINALS"),
    ("callofduty", "callofduty", "Call of Duty"),
    // Not in the curated brief, but needed so variants merge under one readable name.
    ("valorant", "valorant", "VALORANT"),
    ("agenticide", "agenticide", "Agentic IDE"),
];

pub(super) fn normalize(prefix: &str) -> (String, String) {
    let cleaned: String = prefix
        .chars()
        .filter(|c| !matches!(c, '®' | '™' | '©'))
        .map(|c| if c == '-' || c == '_' { ' ' } else { c })
        .collect();
    let display = cleaned.split_whitespace().collect::<Vec<_>>().join(" ");
    let key: String = display.chars().filter(|c| c.is_alphanumeric()).flat_map(char::to_lowercase).collect();
    if key.is_empty() {
        return ("unknown".into(), "Unknown".into());
    }
    match OVERRIDES.iter().find(|(from, _, _)| *from == key) {
        Some((_, merged, name)) => ((*merged).into(), (*name).into()),
        None => (key, display),
    }
}

/// Prefix for names without a date: the text before `__`, else the stem.
pub(super) fn fallback_prefix(file_name: &str) -> &str {
    let stem = match file_name.rsplit_once('.') {
        Some((stem, ext)) if !stem.is_empty() && ext.len() <= 5 && ext.chars().all(|c| c.is_ascii_alphanumeric()) => stem,
        _ => file_name,
    };
    stem.split("__").next().unwrap_or(stem)
}

/// Finds the first `YYYY?MM?DD` (separator `-`, `_` or `.`), then an optional `HH?MM?SS`
/// after one to three `-`/`_`/space/`T` separators. The prefix is the text before the date.
pub(super) fn parse(file_name: &str) -> Option<(String, LocalStamp)> {
    let bytes = file_name.as_bytes();
    for start in 0..bytes.len() {
        if start > 0 && bytes[start - 1].is_ascii_digit() {
            continue;
        }
        let Some(((year, month, day), after)) = date_at(bytes, start) else { continue };
        let (hour, minute, second) = time_after(bytes, after).unwrap_or((0, 0, 0));
        // ASCII digits are never inside a multi-byte char, so `start` is a char boundary.
        let prefix = file_name[..start].trim_end_matches(['_', '-', ' ', '.']).to_owned();
        return Some((prefix, LocalStamp { year, month, day, hour, minute, second }));
    }
    None
}

fn number(bytes: &[u8], at: usize, len: usize) -> Option<u32> {
    let slice = bytes.get(at..at + len)?;
    slice.iter().all(u8::is_ascii_digit).then(|| slice.iter().fold(0u32, |acc, b| acc * 10 + u32::from(b - b'0')))
}

fn date_at(bytes: &[u8], i: usize) -> Option<((i32, u32, u32), usize)> {
    let year = number(bytes, i, 4)? as i32;
    let sep = *bytes.get(i + 4)?;
    if !matches!(sep, b'-' | b'_' | b'.') || bytes.get(i + 7) != Some(&sep) {
        return None;
    }
    let month = number(bytes, i + 5, 2)?;
    let day = number(bytes, i + 8, 2)?;
    if bytes.get(i + 10).is_some_and(u8::is_ascii_digit) || !valid_date(year, month, day) {
        return None;
    }
    Some(((year, month, day), i + 10))
}

fn time_after(bytes: &[u8], mut j: usize) -> Option<(u32, u32, u32)> {
    let mut skipped = 0;
    while skipped < 3 && bytes.get(j).is_some_and(|b| matches!(b, b'-' | b'_' | b' ' | b'T')) {
        j += 1;
        skipped += 1;
    }
    if skipped == 0 {
        return None;
    }
    let hour = number(bytes, j, 2)?;
    let sep = *bytes.get(j + 2)?;
    if !matches!(sep, b'-' | b'_' | b'.' | b':') || bytes.get(j + 5) != Some(&sep) {
        return None;
    }
    let minute = number(bytes, j + 3, 2)?;
    let second = number(bytes, j + 6, 2)?;
    if bytes.get(j + 8).is_some_and(u8::is_ascii_digit) || hour > 23 || minute > 59 || second > 59 {
        return None;
    }
    Some((hour, minute, second))
}
