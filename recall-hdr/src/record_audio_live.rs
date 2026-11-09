//! Audio source changes that reach a running engine without a restart (merge gate p9):
//! - P2-a: `set_source`'s `mic_mute` and `device_id`, and a gain above 0 dB, need an engine
//!   that says `live_source_fields` in its `hello`. An older engine (8111e31) ignores those
//!   fields, so it is restarted for them instead of being sent them.
//! - A4 ruling: an engine track opened on an unsure sibling and unmuted by a fresh Alt+M sends
//!   `source_adopted {id, device_id}`; the endpoint id is saved as the source's own (a config
//!   patch, which then goes live as `set_source device_id`).

use serde_json::Value;

use crate::record_app::Controller;
use crate::record_audio_sources::AudioSource;
use crate::supervisor::Supervisor;

/// The `hello` capability for the fields in [`needs_fields`].
pub const LIVE_SOURCE_FIELDS: &str = "live_source_fields";

/// `line` (a `set_source` from `live_updates`) carries a field an older engine ignores: the
/// mic-mute flag, the saved endpoint id, or a gain above 0 dB (it would not limit it).
pub fn needs_fields(line: &str) -> bool {
    let Ok(cmd) = serde_json::from_str::<Value>(line) else { return true };
    cmd.get("mic_mute").is_some() || cmd.get("device_id").is_some() || cmd.get("gain_db").and_then(Value::as_f64).is_some_and(|db| db > 0.0)
}

/// The running engine cannot apply `lines` live (`live_sources` / `live_fields`: what its
/// `hello` says), so it restarts instead.
pub fn cannot_take(lines: &[String], live_sources: bool, live_fields: bool) -> bool {
    !lines.is_empty() && (!live_sources || (!live_fields && lines.iter().any(|l| needs_fields(l))))
}

/// [`cannot_take`] for the engine `supervisor` runs.
pub fn engine_cannot_take(lines: &[String], supervisor: &Supervisor) -> bool {
    cannot_take(lines, supervisor.engine_takes_live_sources(), supervisor.hello_says(LIVE_SOURCE_FIELDS))
}

/// The config patch that saves `ev`'s endpoint id as its source's own (`source_adopted`), or
/// None: another event, an unknown source, a malformed id, or the id already saved.
pub fn adopt_patch(list: &[AudioSource], ev: &Value) -> Option<Value> {
    if ev.get("type").and_then(Value::as_str) != Some("source_adopted") {
        return None;
    }
    let id = ev.get("id").and_then(Value::as_str)?.trim();
    let device_id = ev.get("device_id").and_then(Value::as_str)?.trim();
    if device_id.is_empty() || device_id.chars().count() > 256 || device_id.chars().any(char::is_control) {
        return None;
    }
    let mut list = list.to_vec();
    let source = list.iter_mut().find(|s| s.id.trim() == id && !s.device.trim().is_empty())?;
    if source.device_id.trim() == device_id {
        return None;
    }
    source.device_id = device_id.to_owned();
    Some(serde_json::json!({"recording": {"audio_sources": list}}))
}

/// Save an adopted endpoint id (see the module notes); other events pass.
pub(crate) fn on_engine_event(ctl: &mut Controller, ev: &Value) {
    if let Some(patch) = adopt_patch(&ctl.file_cfg.recording.audio_sources, ev) {
        crate::log!("record: audio source {} unmuted on another endpoint; saving its id as its own", ev.get("id").and_then(Value::as_str).unwrap_or(""));
        ctl.apply_config_patch(&patch, None);
    }
}

#[cfg(test)]
#[path = "record_audio_live_tests.rs"]
mod tests;
