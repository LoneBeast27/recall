//! A settings summary for a report, built from an allow-list: only the keys below, and only
//! when their value is a number, true/false, or one of the key's fixed words. Lists (games,
//! apps, audio sources, overrides, folder choices) appear only as how many entries they have.
//! No name, path, device, hotkey or free text from the settings file can reach a report: a
//! value that is not of its key's kind is written as `?`.

/// What a summarised value may be.
#[derive(Clone, Copy)]
enum Kind {
    Bool,
    Number,
    /// One of these words.
    Word(&'static [&'static str]),
}

const B: Kind = Kind::Bool;
const N: Kind = Kind::Number;

/// `section.key` and the value kind it may show (word sets as `record_config.rs` and
/// `config_export.rs` define them).
const KEYS: &[(&str, Kind)] = &[
    ("version", N),
    ("behavior.hdr_auto", B),
    ("behavior.debounce_ms", N),
    ("behavior.sdr_white_nits", N),
    ("startup.autostart", B),
    ("recording.auto_record", B),
    ("recording.buffer_secs", N),
    ("recording.codec", Kind::Word(&["h264", "hevc", "av1"])),
    ("recording.fps", N),
    ("recording.bitrate_mbps", N),
    ("recording.quality", Kind::Word(&["performance", "balanced", "quality"])),
    ("recording.record_desktop_when_idle", B),
    ("recording.mic_enabled", B),
    ("overlay.hidden", B),
    ("overlay.enabled", B),
    ("overlay.corner", Kind::Word(&["dynamic", "top-left", "top-right", "bottom-left", "bottom-right"])),
    ("overlay.corner_version", N),
    ("overlay.stats", B),
    ("overlay.stats_style", Kind::Word(&["minimal"])),
    ("storage.cap_gb", N),
    ("export.fps", N),
    ("export.height", N),
    ("export.max_mb", N),
    ("export.mode", Kind::Word(&["reencode", "lossless"])),
];

/// Tables and lists shown only as a count of entries: `[[table]]` blocks, `[table]` entries,
/// or the items of an inline `key = [ … ]` list.
const COUNTED: &[&str] = &["recording.audio_sources", "recording.save_hotkeys", "overrides", "recording.game_folder_choices", "detect.fullscreen_games", "recording.app_audio"];

/// The summary of a settings file's text.
pub fn summary(text: &str) -> String {
    let mut section = String::new();
    let mut lines: Vec<String> = Vec::new();
    let mut counts: Vec<(&str, usize)> = COUNTED.iter().map(|c| (*c, 0)).collect();
    let bump = |name: &str, by: usize, counts: &mut Vec<(&str, usize)>| {
        if let Some(c) = counts.iter_mut().find(|(n, _)| *n == name) {
            c.1 += by;
        }
    };
    // A list that spans lines: (its full key, the text gathered so far).
    let mut open_list: Option<(String, String)> = None;
    for raw in text.lines() {
        if let Some((key, mut gathered)) = open_list.take() {
            gathered.push_str(raw);
            gathered.push('\n');
            if list_closed(&gathered) {
                bump(&key, list_items(&gathered), &mut counts);
            } else {
                open_list = Some((key, gathered));
            }
            continue;
        }
        let line = without_comment(raw).trim();
        if line.starts_with("[[") {
            section = line.trim_matches(['[', ']']).trim().to_string();
            bump(&section, 1, &mut counts);
            continue;
        }
        if line.starts_with('[') {
            section = line.trim_matches(['[', ']']).trim().to_string();
            continue;
        }
        let Some((key, value)) = line.split_once('=') else { continue };
        let (key, value) = (key.trim(), value.trim());
        if section == "recording.game_folder_choices" {
            bump(&section, 1, &mut counts);
            continue;
        }
        let full = if section.is_empty() { key.to_string() } else { format!("{section}.{key}") };
        if value.starts_with('[') {
            if list_closed(value) {
                bump(&full, list_items(value), &mut counts);
            } else {
                open_list = Some((full, format!("{value}\n")));
            }
            continue;
        }
        if let Some((name, kind)) = KEYS.iter().find(|(k, _)| *k == full) {
            lines.push(format!("{name} = {}", shown(value, *kind)));
        }
    }
    for (name, count) in counts {
        lines.push(format!("{name}: {count} entries"));
    }
    lines.join("\n") + "\n"
}

/// The value as it may appear, or `?`.
fn shown(value: &str, kind: Kind) -> String {
    let value = value.split('#').next().unwrap_or("").trim();
    let ok = match kind {
        Kind::Bool => value == "true" || value == "false",
        Kind::Number => !value.is_empty() && value.len() <= 12 && value.chars().all(|c| c.is_ascii_digit() || c == '.' || c == '-'),
        Kind::Word(words) => words.contains(&value.trim_matches('"')),
    };
    if ok {
        value.trim_matches('"').to_string()
    } else {
        "?".into()
    }
}

/// The line without a `#` comment (a `#` inside a quoted string stays).
fn without_comment(line: &str) -> &str {
    let mut quote: Option<char> = None;
    let mut escaped = false;
    for (i, c) in line.char_indices() {
        match (quote, c) {
            (Some('"'), '\\') if !escaped => {
                escaped = true;
                continue;
            }
            (Some(q), c) if c == q && !escaped => quote = None,
            (None, '"' | '\'') => quote = Some(c),
            (None, '#') => return &line[..i],
            _ => {}
        }
        escaped = false;
    }
    line
}

/// The list's items: its quoted strings, or (with none) its comma-separated values. Commas
/// inside strings and comments do not count.
fn list_items(list: &str) -> usize {
    let mut strings = 0;
    let mut values = 0;
    let mut current = false;
    for line in list.lines() {
        let mut quote: Option<char> = None;
        let mut escaped = false;
        for c in without_comment(line).chars() {
            match (quote, c) {
                (Some('"'), '\\') if !escaped => {
                    escaped = true;
                    continue;
                }
                (Some(q), c) if c == q && !escaped => {
                    quote = None;
                    strings += 1;
                }
                (Some(_), _) => {}
                (None, '"' | '\'') => quote = Some(c),
                (None, ',' | ']') => {
                    values += usize::from(current);
                    current = false;
                }
                (None, c) if !c.is_whitespace() && c != '[' => current = true,
                _ => {}
            }
            escaped = false;
        }
    }
    if strings > 0 {
        strings
    } else {
        values
    }
}

/// The list's closing `]` (outside strings and comments) has been read.
fn list_closed(list: &str) -> bool {
    let mut depth = 0i32;
    for line in list.lines() {
        let mut quote: Option<char> = None;
        let mut escaped = false;
        for c in without_comment(line).chars() {
            match (quote, c) {
                (Some('"'), '\\') if !escaped => {
                    escaped = true;
                    continue;
                }
                (Some(q), c) if c == q && !escaped => quote = None,
                (Some(_), _) => {}
                (None, '"' | '\'') => quote = Some(c),
                (None, '[') => depth += 1,
                (None, ']') => {
                    depth -= 1;
                    if depth == 0 {
                        return true;
                    }
                }
                _ => {}
            }
            escaped = false;
        }
    }
    false
}
