//! Load-time repair of config.toml. `record_config::validate` guards `set_config` patches; a
//! loaded file never went through it, so one bad value (a hand edit, or a file saved by an
//! older build with fps up to 960) reached the engine and made every later patch fail on a
//! key the user never touched. Here each value is repaired on its own, with one log line:
//! - numbers out of range clamp to the nearest valid value (a loaded fps above 240 is 240);
//! - an unknown token takes its default (a case-only difference is fixed instead);
//! - an invalid list entry (audio source, app, fullscreen game, hotkey preset) is dropped;
//! - a hotkey that does not parse or repeats an earlier one is turned off (the mic shortcut,
//!   which must have one, gets its default).
//!
//! The rest of the file is always kept. The result passes `record_config::validate`.

use crate::config::Config;
use crate::log;
use crate::record_config::{
    self as rc, BITRATE_MBPS_MAX, BITRATE_MBPS_MIN, BUFFER_SECS_MAX, BUFFER_SECS_MIN, CODECS, CORNERS, FPS_MAX, FPS_MIN,
    QUALITIES, STATS_STYLES, STORAGE_CAP_MIN_GB,
};

/// Repair `cfg` in place; returns one note per repair (each is also logged).
pub fn sanitize_loaded(cfg: &mut Config) -> Vec<String> {
    let mut notes = Vec::new();
    numbers(cfg, &mut notes);
    tokens(cfg, &mut notes);
    text_fields(cfg, &mut notes);
    crate::config_export::repair(&mut cfg.export, &mut notes);
    crate::config_validate_lists::hotkeys(&mut cfg.recording, &mut notes);
    crate::config_validate_lists::audio_sources(&mut cfg.recording.audio_sources, &mut notes);
    crate::config_validate_lists::app_audio(&mut cfg.recording.app_audio, &mut notes);
    for note in &notes {
        log!("config: repaired {note}");
    }
    if let Err(e) = rc::validate(cfg) {
        // Not expected: every rule `validate` checks has a repair above.
        log!("config: still invalid after repair: {e}");
    }
    notes
}

fn clamp_u32(key: &str, value: &mut u32, min: u32, max: u32, notes: &mut Vec<String>) {
    let fixed = (*value).clamp(min, max);
    if fixed != *value {
        notes.push(format!("{key} {} -> {fixed} (allowed {min}..={max})", *value));
        *value = fixed;
    }
}

fn numbers(cfg: &mut Config, notes: &mut Vec<String>) {
    let rec = &mut cfg.recording;
    clamp_u32("recording.buffer_secs", &mut rec.buffer_secs, BUFFER_SECS_MIN, BUFFER_SECS_MAX, notes);
    clamp_u32("recording.fps", &mut rec.fps, FPS_MIN, FPS_MAX, notes);
    clamp_u32("recording.bitrate_mbps", &mut rec.bitrate_mbps, BITRATE_MBPS_MIN, BITRATE_MBPS_MAX, notes);
    let cap = cfg.storage.cap_gb;
    if (1..STORAGE_CAP_MIN_GB).contains(&cap) {
        // A cap was meant: the floor, never off (off lets the folder grow until the disk fills).
        notes.push(format!("storage.cap_gb {cap} -> {STORAGE_CAP_MIN_GB} (0 = off, else at least {STORAGE_CAP_MIN_GB})"));
        cfg.storage.cap_gb = STORAGE_CAP_MIN_GB;
    }
}

/// `value` as one of `allowed`: kept, re-cased (`HEVC` -> `hevc`, `Top_Right` -> `top-right`),
/// else `default`.
fn token(key: &str, value: &mut String, allowed: &[&str], default: &str, notes: &mut Vec<String>) {
    if allowed.contains(&value.as_str()) {
        return;
    }
    let folded = value.trim().to_ascii_lowercase().replace('_', "-");
    let fixed = allowed.iter().find(|a| **a == folded).map_or(default, |a| *a);
    notes.push(format!("{key} '{value}' -> '{fixed}'"));
    *value = fixed.to_owned();
}

fn tokens(cfg: &mut Config, notes: &mut Vec<String>) {
    let rec = &mut cfg.recording;
    token("recording.codec", &mut rec.codec, &CODECS, "h264", notes);
    token("recording.quality", &mut rec.quality, &QUALITIES, "balanced", notes);
    token("overlay.corner", &mut cfg.overlay.corner, &CORNERS, "dynamic", notes);
    token("overlay.stats_style", &mut cfg.overlay.stats_style, &STATS_STYLES, "minimal", notes);
    if rc::parse_cpu_mask(&rec.cpu_mask).is_err() {
        notes.push(format!("recording.cpu_mask '{}' -> 'auto'", rec.cpu_mask));
        rec.cpu_mask = "auto".into();
    }
}

/// At most `max` characters and no control characters (newlines included).
pub(crate) fn one_line(text: &str, max: usize) -> bool {
    text.chars().count() <= max && !text.chars().any(char::is_control)
}

/// An exe file name: no folders, no drive, one line.
pub(crate) fn exe_name(text: &str) -> bool {
    let exe = text.trim();
    !exe.is_empty() && exe.len() <= 64 && !exe.chars().any(|c| matches!(c, '\\' | '/' | ':') || c.is_control())
}

fn clear(key: &str, value: &mut String, notes: &mut Vec<String>) {
    notes.push(format!("{key} '{}' cleared (not valid there)", value.escape_debug()));
    value.clear();
}

fn text_fields(cfg: &mut Config, notes: &mut Vec<String>) {
    let rec = &mut cfg.recording;
    if rec.output_dir.trim().is_empty() {
        rec.output_dir = rc::default_output_dir();
        notes.push(format!("recording.output_dir was empty -> '{}'", rec.output_dir));
    }
    for (key, name) in [("recording.mic_device", &mut rec.mic_device), ("recording.desktop_audio_device", &mut rec.desktop_audio_device)] {
        // Bytes, as `record_config::validate` counts them (a long non-ASCII name is > 256 bytes).
        if name.len() > 256 || name.chars().any(char::is_control) {
            clear(key, name, notes);
        }
    }
    if !rec.voice_app.trim().is_empty() && !exe_name(&rec.voice_app) {
        clear("recording.voice_app", &mut rec.voice_app, notes);
    }
    if cfg.midi.input.len() > 256 || cfg.midi.input.chars().any(char::is_control) {
        clear("midi.input", &mut cfg.midi.input, notes);
    }
    cfg.detect.fullscreen_games.retain(|g| {
        let ok = !g.trim().is_empty() && !g.contains(['\\', '/']);
        if !ok {
            notes.push(format!("detect.fullscreen_games entry '{g}' dropped (an exe name like Discord.exe)"));
        }
        ok
    });
}

#[cfg(test)]
#[path = "config_validate_tests.rs"]
mod tests;
