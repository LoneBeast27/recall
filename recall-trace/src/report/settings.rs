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
    for raw in text.lines() {
        let line = raw.trim();
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
            let items = value.trim_matches(['[', ']']).split(',').filter(|i| !i.trim().is_empty()).count();
            bump(&full, items, &mut counts);
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
