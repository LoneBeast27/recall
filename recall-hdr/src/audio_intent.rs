//! The controller's audio intent as engine lines (design-audio-mute §3). Intent is state, never
//! only a diff: at attach the whole intent is restated before `sync` ([`restatement`]), and an
//! Audio-page change sends every privacy-tightening part live ([`change_lines`]) before any
//! restart is decided. Only the track shape (a new track, a newly picked device) waits for a
//! restart; a removed, disabled or re-picked source is `retire`d at once (the engine stores
//! silence for it and closes its stream).
//!
//! `mic_mute` is always sent as the effective flag: an input with no flag is muted by Alt+M
//! (true), so dropping the flag goes live too.

use serde_json::{json, Map, Value};

use crate::record_audio_sources::{engine_list, track_shape, AudioSource};

/// The flag the engine applies for `s`: an input with no flag is muted by Alt+M; None for a
/// source Alt+M never mutes (an output, the game, an app).
pub fn effective_mic_mute(s: &AudioSource) -> Option<bool> {
    (s.kind == "input").then(|| s.mic_mute.unwrap_or(true))
}

/// The engine list (`RECALL_AUDIO_SOURCES`, `engine_value`) back as sources; None when unreadable.
pub fn parse(value: &str) -> Option<Vec<AudioSource>> {
    serde_json::from_str(value).ok()
}

/// One source's track as the engine opens it (no gain, name, flag or saved id).
fn shape_of(s: &AudioSource) -> AudioSource {
    track_shape(std::slice::from_ref(s)).pop().unwrap_or_else(|| s.clone())
}

/// The same tracks, in the same order (no restart needed).
pub fn same_shape(a: &[AudioSource], b: &[AudioSource]) -> bool {
    track_shape(a) == track_shape(b)
}

fn retire(id: &str) -> Value {
    json!({"cmd": "set_source", "id": id, "retire": true})
}

/// The full state of `s` as one `set_source`: gain, name, effective flag and saved id.
fn state_line(s: &AudioSource) -> Value {
    let mut cmd = Map::new();
    cmd.insert("cmd".into(), "set_source".into());
    cmd.insert("id".into(), s.id.clone().into());
    cmd.insert("gain_db".into(), json!(s.gain_db));
    cmd.insert("name".into(), s.name.clone().into());
    if let Some(on) = effective_mic_mute(s) {
        cmd.insert("mic_mute".into(), on.into());
    }
    cmd.insert("device_id".into(), s.device_id.clone().into());
    Value::Object(cmd)
}

/// The `retire` lines for every track in `old` that `new` no longer records as the same track:
/// removed, disabled, or re-picked under another device name (the device it opened is no longer
/// picked, B3 ruling).
fn retired(old: &[AudioSource], new: &[AudioSource]) -> Vec<Value> {
    old.iter().filter(|o| !new.iter().any(|n| n.id.trim() == o.id.trim() && shape_of(n) == shape_of(o))).map(|o| retire(o.id.trim())).collect()
}

/// The restatement queued at attach, before `sync`: the state of every source in `current`, and
/// `retire` for each track the engine was `baked` with that `current` no longer records the
/// same way. Both lists are engine lists (enabled only).
pub fn restatement(baked: &[AudioSource], current: &[AudioSource]) -> Vec<Value> {
    current.iter().map(state_line).chain(retired(baked, current)).collect()
}

/// The lines an Audio-page change sends to the running engine, whatever restart follows: for
/// each source in both lists, what changed (gain, name, effective flag, saved id), and `retire`
/// for every track that is no longer recorded as it was. Config lists (disabled included).
pub fn change_lines(old: &[AudioSource], new: &[AudioSource]) -> Vec<Value> {
    let (old, new) = (engine_list(old), engine_list(new));
    let mut lines: Vec<Value> = new
        .iter()
        .filter_map(|n| {
            let o = old.iter().find(|o| o.id == n.id && shape_of(o) == shape_of(n))?;
            let mut cmd = Map::new();
            if o.gain_db != n.gain_db {
                cmd.insert("gain_db".into(), json!(n.gain_db));
            }
            if o.name != n.name {
                cmd.insert("name".into(), n.name.clone().into());
            }
            if let Some(on) = effective_mic_mute(n).filter(|_| effective_mic_mute(o) != effective_mic_mute(n)) {
                cmd.insert("mic_mute".into(), on.into());
            }
            if o.device_id != n.device_id {
                cmd.insert("device_id".into(), n.device_id.clone().into());
            }
            (!cmd.is_empty()).then(|| {
                cmd.insert("cmd".into(), "set_source".into());
                cmd.insert("id".into(), n.id.clone().into());
                Value::Object(cmd)
            })
        })
        .collect();
    lines.extend(retired(&old, &new));
    lines
}

#[cfg(test)]
#[path = "audio_intent_tests.rs"]
mod tests;
