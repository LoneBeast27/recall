//! The list parts of the load-time repair (`config_validate`): hotkeys, audio sources and the
//! old app list. A bad entry is dropped (or a bad shortcut turned off), never the whole list.

use std::collections::HashMap;

use crate::config_validate::{exe_name, one_line};
use crate::hotkey::parse_accel;
use crate::record_app_audio::{AppAudioSource, APP_AUDIO_MAX};
use crate::record_audio_sources::{AudioSource, GAIN_MAX_DB, GAIN_MIN_DB, KINDS, SOURCES_MAX};
use crate::record_config::{RecordingCfg, BUFFER_SECS_MAX};
use crate::record_hotkeys::{legacy_save_hotkey, MAX_SAVE_HOTKEYS};

/// Where the mic shortcut goes when its own combo does not parse or a save preset holds it, in
/// order. More entries than `MAX_SAVE_HOTKEYS`, so one is always free.
pub(crate) const MIC_FALLBACKS: [&str; 10] =
    ["Alt+M", "Ctrl+Alt+M", "Alt+Shift+M", "Ctrl+Shift+M", "Ctrl+Alt+Shift+M", "Alt+F8", "Alt+F9", "Alt+F10", "Alt+F11", "Alt+F12"];

/// Combos already taken, by `HotKey::id`, with their config key (for the note).
struct Held(HashMap<u32, String>);

impl Held {
    /// Take `accel` for `key`; the reason it can't be taken, if any.
    fn take(&mut self, key: &str, accel: &str) -> Result<(), String> {
        let hk = parse_accel(accel)?;
        if let Some(owner) = self.0.get(&hk.id()) {
            return Err(format!("'{accel}' is already {owner}"));
        }
        self.0.insert(hk.id(), key.to_owned());
        Ok(())
    }
}

/// The save presets first (the order `record_hotkeys::bindings` binds them in, so a clash keeps
/// Save as it always did), then the mic (it must keep a key, so it moves), then the optional ones.
pub(crate) fn hotkeys(rec: &mut RecordingCfg, notes: &mut Vec<String>) {
    let mut held = Held(HashMap::new());
    save_presets(rec, &mut held, notes);
    mic(rec, &mut held, notes);
    let singles = [
        ("recording.bookmark_hotkey", &mut rec.bookmark_hotkey),
        ("recording.screenshot_hotkey", &mut rec.screenshot_hotkey),
        ("recording.record_hotkey", &mut rec.record_hotkey),
        ("recording.stats_hotkey", &mut rec.stats_hotkey),
        ("recording.overlay_hotkey", &mut rec.overlay_hotkey),
        ("recording.editor_hotkey", &mut rec.editor_hotkey),
    ];
    for (key, accel) in singles {
        if accel.trim().is_empty() {
            continue;
        }
        if let Err(why) = held.take(key, accel) {
            notes.push(format!("{key} '{accel}' turned off: {why}"));
            accel.clear();
        }
    }
    rec.save_hotkey = legacy_save_hotkey(&rec.save_hotkeys);
}

/// The mic shortcut keeps its combo when it parses and is free, else takes the first free one
/// of `MIC_FALLBACKS` (Alt+M first).
fn mic(rec: &mut RecordingCfg, held: &mut Held, notes: &mut Vec<String>) {
    let Err(why) = held.take("recording.mic_hotkey", &rec.mic_hotkey) else { return };
    let Some(free) = MIC_FALLBACKS.iter().find(|f| held.take("recording.mic_hotkey", f).is_ok()) else {
        notes.push(format!("recording.mic_hotkey '{}' kept: no free fallback ({why})", rec.mic_hotkey));
        return;
    };
    notes.push(format!("recording.mic_hotkey '{}' -> '{free}': {why}", rec.mic_hotkey));
    rec.mic_hotkey = (*free).to_owned();
}

/// At most `MAX_SAVE_HOTKEYS`; a preset that does not parse or repeats an earlier one goes.
fn save_presets(rec: &mut RecordingCfg, held: &mut Held, notes: &mut Vec<String>) {
    if rec.save_hotkeys.len() > MAX_SAVE_HOTKEYS {
        notes.push(format!("recording.save_hotkeys: kept the first {MAX_SAVE_HOTKEYS} of {}", rec.save_hotkeys.len()));
        rec.save_hotkeys.truncate(MAX_SAVE_HOTKEYS);
    }
    let mut i = 0;
    rec.save_hotkeys.retain_mut(|s| {
        let key = format!("recording.save_hotkeys[{i}]");
        i += 1;
        if let Err(why) = held.take(&key, &s.hotkey) {
            notes.push(format!("{key} '{}' dropped: {why}", s.hotkey));
            return false;
        }
        if s.seconds > BUFFER_SECS_MAX {
            notes.push(format!("{key}.seconds {} -> {BUFFER_SECS_MAX}", s.seconds));
            s.seconds = BUFFER_SECS_MAX;
        }
        true
    });
}

/// Why `s` can't be recorded as it is, after the fixable parts (kind spelling, gain) are fixed.
fn source_problem(s: &mut AudioSource, ids: &mut Vec<String>, has_game: &mut bool) -> Option<String> {
    let id = s.id.trim().to_lowercase();
    if id.is_empty() || !one_line(&id, 32) {
        return Some("needs an id of one line, at most 32 characters".into());
    }
    if ids.contains(&id) {
        return Some("repeats an earlier id".into());
    }
    let kind = s.kind.trim().to_ascii_lowercase();
    let Some(kind) = KINDS.iter().find(|k| **k == kind) else {
        return Some(format!("kind '{}' is not one of {}", s.kind, KINDS.join(" | ")));
    };
    s.kind = (*kind).to_owned();
    if s.name.trim().is_empty() || !one_line(&s.name, 64) {
        return Some("needs a name of one line, at most 64 characters".into());
    }
    if !one_line(&s.device, 256) {
        return Some("device must be one line of at most 256 characters".into());
    }
    if s.kind == "app" && !exe_name(&s.exe) {
        return Some("needs an exe file name (no folders)".into());
    }
    if s.kind == "game" {
        if *has_game {
            return Some("is a second game source".into());
        }
        *has_game = true;
    }
    s.gain_db = if s.gain_db.is_nan() { 0.0 } else { s.gain_db.clamp(GAIN_MIN_DB, GAIN_MAX_DB) };
    ids.push(id);
    None
}

/// Drop the entries the engine would reject (one of them used to cost every track, Game
/// included), fix a kind's spelling, clamp gain, keep the first `SOURCES_MAX`.
pub(crate) fn audio_sources(list: &mut Vec<AudioSource>, notes: &mut Vec<String>) {
    let (mut ids, mut has_game) = (Vec::new(), false);
    list.retain_mut(|s| {
        let (kind, gain) = (s.kind.clone(), s.gain_db);
        match source_problem(s, &mut ids, &mut has_game) {
            Some(why) => {
                notes.push(format!("recording.audio_sources '{}' dropped: {why}", s.id));
                false
            }
            None => {
                if kind != s.kind {
                    notes.push(format!("recording.audio_sources '{}' kind '{kind}' -> '{}'", s.id, s.kind));
                }
                if gain.to_bits() != s.gain_db.to_bits() {
                    notes.push(format!("recording.audio_sources '{}' gain_db {gain} -> {}", s.id, s.gain_db));
                }
                true
            }
        }
    });
    if list.len() > SOURCES_MAX {
        notes.push(format!("recording.audio_sources: kept the first {SOURCES_MAX} of {}", list.len()));
        list.truncate(SOURCES_MAX);
    }
}

/// The old app list (read only to build `audio_sources`): drop bad or repeated entries.
pub(crate) fn app_audio(list: &mut Vec<AppAudioSource>, notes: &mut Vec<String>) {
    let mut seen = Vec::new();
    list.retain(|app| {
        let key = app.exe.trim().to_lowercase();
        let key = key.strip_suffix(".exe").unwrap_or(&key).to_owned();
        let ok = exe_name(&app.exe) && one_line(&app.label, 64) && !seen.contains(&key);
        if ok {
            seen.push(key);
        } else {
            notes.push(format!("recording.app_audio '{}' dropped", app.exe));
        }
        ok
    });
    if list.len() > APP_AUDIO_MAX {
        notes.push(format!("recording.app_audio: kept the first {APP_AUDIO_MAX} of {}", list.len()));
        list.truncate(APP_AUDIO_MAX);
    }
}
