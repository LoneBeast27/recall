//! `[[recording.audio_sources]]`: every track a clip records, in track order (the Audio
//! page's strips). Each source is one track named `name`:
//! - `game`: the recorded game's process tree; on the desktop, loopback of `device`
//!   ("" = the Windows default output). At most one.
//! - `output`: loopback of an output device ("" = the Windows default output).
//! - `input`: a microphone or line-in ("" = the Windows default input).
//! - `app`: process loopback of one app by `exe`.
//!
//! `gain_db` is applied by the engine before the AAC encode; `enabled = false` keeps the
//! source in the list but records no track. Passed to the engine as `RECALL_AUDIO_SOURCES`
//! (the enabled sources as JSON). A config written before this list existed gets one built
//! from its old keys (`desktop_audio_device`, `mic_device`/`mic_enabled`, `voice_app`,
//! `app_audio`), so saved choices carry over.

use serde::{Deserialize, Serialize};

use crate::record_config::RecordingCfg;

pub const SOURCES_ENV: &str = "RECALL_AUDIO_SOURCES";
/// Most sources a recording carries (each is one capture stream and one track).
pub const SOURCES_MAX: usize = 8;
pub const KINDS: [&str; 4] = ["game", "output", "input", "app"];
pub const GAIN_MIN_DB: f32 = -60.0;
pub const GAIN_MAX_DB: f32 = 12.0;

/// One recorded source.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct AudioSource {
    /// Stable identity (unique in the list); the engine's internal track name.
    pub id: String,
    /// `game` | `output` | `input` | `app`.
    pub kind: String,
    /// Endpoint friendly name for `game`/`output`/`input` ("" = the Windows default).
    pub device: String,
    /// The endpoint id (`IMMDevice::GetId`) the Audio page saved with `device` ("" = none). A
    /// line-in skips Alt+M only on the endpoint with this id (R5-deep-audio-1); it never picks
    /// which endpoint opens.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub device_id: String,
    /// Image file name for `app`, e.g. `player.exe`.
    pub exe: String,
    /// Track name in clips.
    pub name: String,
    pub enabled: bool,
    /// Recorded gain, `GAIN_MIN_DB..=GAIN_MAX_DB`.
    pub gain_db: f32,
    /// Whether the mic-mute hotkey silences this `input`: `true` a microphone, `false` a line-in
    /// or capture card (muting the mic leaves its sound). Absent (every config written before
    /// the flag, a new default source): muted, as before the flag (R3-deep-audio-1); the
    /// endpoint's Windows form factor never decides. Saved only when the strip says.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mic_mute: Option<bool>,
    /// An unpinned output follows Windows' communications role.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub communications: bool,
    /// The audio strip's selected glyph. Cosmetic and never sent to the engine.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub icon: String,
}

impl Default for AudioSource {
    fn default() -> Self {
        AudioSource { id: String::new(), kind: "input".into(), device: String::new(), device_id: String::new(), exe: String::new(), name: String::new(), enabled: true, gain_db: 0.0, mic_mute: None, communications: false, icon: String::new() }
    }
}

fn source(id: &str, kind: &str, name: &str) -> AudioSource {
    AudioSource { id: id.into(), kind: kind.into(), name: name.into(), ..AudioSource::default() }
}

/// Fallback settings when a profile could not be read. Keep game-only capture there.
pub fn default_sources() -> Vec<AudioSource> {
    vec![source("game", "game", "Game"), source("mic", "input", "Microphone")]
}

/// The Windows default capture endpoint at setup time, as Windows reports it.
#[derive(Debug, Clone, PartialEq)]
pub struct CaptureEndpoint {
    pub id: String,
    pub name: String,
}

/// The first-run profile (the maintainer: "auto mic and game audio"): the detected game's own audio
/// and the default microphone, pinned by the endpoint id and name Windows reports now (no
/// name literals; with no capture device, the Windows default input). No System output, and
/// no Communications output: a call stays out of the clip and its Mix.
fn initial_sources(mic: Option<CaptureEndpoint>) -> Vec<AudioSource> {
    let mut microphone = source("mic", "input", "Microphone");
    if let Some(endpoint) = mic {
        microphone.device = endpoint.name;
        microphone.device_id = endpoint.id;
    }
    vec![source("game", "game", "Game"), microphone]
}

pub fn initialize_sources() -> Vec<AudioSource> {
    let joined = wasapi::initialize_mta().is_ok();
    let mic = default_capture();
    if joined {
        wasapi::deinitialize();
    }
    initial_sources(mic)
}

/// The Windows default capture endpoint (console role), `None` when there is none.
fn default_capture() -> Option<CaptureEndpoint> {
    let device = wasapi::get_default_device(&wasapi::Direction::Capture).ok()?;
    let (id, name) = (device.get_id().ok()?, device.get_friendlyname().ok()?);
    (!id.trim().is_empty() && !name.trim().is_empty()).then(|| CaptureEndpoint { id, name })
}

/// The list a config without `audio_sources` stands for, from its old keys.
pub fn from_legacy(rec: &RecordingCfg) -> Vec<AudioSource> {
    let mut list = vec![
        AudioSource { device: rec.desktop_audio_device.trim().to_owned(), ..source("game", "game", "Game") },
        AudioSource { device: rec.mic_device.trim().to_owned(), enabled: rec.mic_enabled, ..source("mic", "input", "Microphone") },
    ];
    let voice = rec.voice_app.trim();
    if !voice.is_empty() {
        list.push(AudioSource { exe: voice.to_owned(), ..source("voice", "app", "Voice chat") });
    }
    for (n, app) in rec.app_audio.iter().enumerate() {
        let exe = app.exe.trim();
        if list.iter().any(|s| s.kind == "app" && same_exe(&s.exe, exe)) {
            continue;
        }
        let label = app.label.trim();
        let name = if label.is_empty() { exe_stem(exe) } else { label.to_owned() };
        list.push(AudioSource { exe: exe.to_owned(), ..source(&format!("app-{}", n + 1), "app", &name) });
    }
    list.truncate(SOURCES_MAX);
    list
}

/// Build the list from the old keys when the loaded TOML has none.
pub fn migrate_loaded(rec: &mut RecordingCfg, toml_text: &str) {
    let has_list = toml::from_str::<toml::Value>(toml_text)
        .ok()
        .and_then(|v| v.get("recording").map(|r| r.get("audio_sources").is_some()))
        .unwrap_or(false);
    if !has_list {
        rec.audio_sources = from_legacy(rec);
    }
}

fn one_line(text: &str, max: usize) -> bool {
    text.chars().count() <= max && !text.chars().any(char::is_control)
}

/// Unique ids, at most `SOURCES_MAX`, known kinds, one game, an exe for every app,
/// one-line names and devices, gain in range.
pub fn validate(list: &[AudioSource]) -> Result<(), String> {
    if list.len() > SOURCES_MAX {
        return Err(format!("recording.audio_sources holds at most {SOURCES_MAX} sources, got {}", list.len()));
    }
    let mut ids = std::collections::HashSet::new();
    for s in list {
        let id = s.id.trim();
        if id.is_empty() || !one_line(id, 32) || !ids.insert(id.to_lowercase()) {
            return Err(format!("recording.audio_sources id '{id}' must be unique, one line, at most 32 characters"));
        }
        if !KINDS.contains(&s.kind.as_str()) {
            return Err(format!("recording.audio_sources '{id}' kind '{}' must be one of {}", s.kind, KINDS.join(" | ")));
        }
        if s.name.trim().is_empty() || !one_line(&s.name, 64) {
            return Err(format!("recording.audio_sources '{id}' name must be one line of 1 to 64 characters"));
        }
        if !one_line(&s.device, 256) {
            return Err(format!("recording.audio_sources '{id}' device must be one line of at most 256 characters"));
        }
        if !one_line(&s.device_id, 256) {
            return Err(format!("recording.audio_sources '{id}' device_id must be one line of at most 256 characters"));
        }
        let exe = s.exe.trim();
        if s.kind == "app" && (exe.is_empty() || exe.len() > 64 || exe.chars().any(|c| matches!(c, '\\' | '/' | ':') || c.is_control())) {
            return Err(format!("recording.audio_sources '{id}' needs an exe file name (no folders)"));
        }
        if !(GAIN_MIN_DB..=GAIN_MAX_DB).contains(&s.gain_db) {
            return Err(format!("recording.audio_sources '{id}' gain_db must be {GAIN_MIN_DB}..={GAIN_MAX_DB}"));
        }
    }
    if list.iter().filter(|s| s.kind == "game").count() > 1 {
        return Err("recording.audio_sources holds at most one game source".into());
    }
    Ok(())
}

/// What the engine records: the enabled sources, trimmed.
fn engine_list(list: &[AudioSource]) -> Vec<AudioSource> {
    list.iter()
        .filter(|s| s.enabled)
        .map(|s| AudioSource { id: s.id.trim().into(), device: s.device.trim().into(), device_id: s.device_id.trim().into(), exe: s.exe.trim().into(), name: s.name.trim().into(), icon: String::new(), ..s.clone() })
        .collect()
}

/// The engine's tracks without what it can change live (gain, name, mic-mute flag, and the
/// endpoint id saved with an unchanged device name: P1-B, a new id is sent live and the track
/// reopens on it, so unmuting a line-in or picking the same device again keeps the buffer).
fn track_shape(list: &[AudioSource]) -> Vec<AudioSource> {
    engine_list(list).into_iter().map(|s| AudioSource { gain_db: 0.0, name: String::new(), mic_mute: None, device_id: String::new(), ..s }).collect()
}

/// A recorded source lost its mic-mute flag (back to none, muted): `set_source`
/// cannot say "no flag", so only a restart applies it. Setting a flag goes live.
fn flag_dropped(old: &[AudioSource], new: &[AudioSource]) -> bool {
    let old = engine_list(old);
    engine_list(new).iter().any(|n| n.mic_mute.is_none() && old.iter().any(|o| o.id == n.id && o.mic_mute.is_some()))
}

/// Different tracks for the engine (a restart)? Order matters: it is the clip's track order.
/// A gain, a name or the mic-mute flag alone is not: [`live_updates`] sends those to the running
/// engine. A gain above 0 dB is live too: a long-recording track runs the -1 dBFS limiter from
/// its first sample above the ceiling (`longrec::machine`), and a replay save limits a track that
/// has been boosted.
pub fn changed(old: &[AudioSource], new: &[AudioSource]) -> bool {
    track_shape(old) != track_shape(new) || flag_dropped(old, new)
}

/// `set_source` lines for the recorded sources whose gain, name or mic-mute flag changed (the
/// running engine applies them to its tracks; the replay buffer stays). Only meaningful when
/// [`changed`] is false.
pub fn live_updates(old: &[AudioSource], new: &[AudioSource]) -> Vec<String> {
    let old = engine_list(old);
    engine_list(new)
        .into_iter()
        .filter_map(|n| {
            let o = old.iter().find(|o| o.id == n.id)?;
            let mut cmd = serde_json::Map::new();
            cmd.insert("cmd".into(), "set_source".into());
            cmd.insert("id".into(), n.id.clone().into());
            if o.gain_db != n.gain_db {
                cmd.insert("gain_db".into(), serde_json::json!(n.gain_db));
            }
            if o.name != n.name {
                cmd.insert("name".into(), n.name.clone().into());
            }
            if let Some(on) = n.mic_mute.filter(|_| o.mic_mute != n.mic_mute) {
                cmd.insert("mic_mute".into(), on.into());
            }
            if o.device_id != n.device_id {
                cmd.insert("device_id".into(), n.device_id.clone().into());
            }
            (cmd.len() > 2).then(|| serde_json::Value::Object(cmd).to_string())
        })
        .collect()
}

/// `RECALL_AUDIO_SOURCES`: the enabled sources as JSON (`[]` when none: no audio tracks).
pub fn engine_value(list: &[AudioSource]) -> String {
    serde_json::to_string(&engine_list(list)).unwrap_or_else(|_| "[]".into())
}

fn same_exe(a: &str, b: &str) -> bool {
    exe_stem(a).eq_ignore_ascii_case(&exe_stem(b))
}

/// `player.exe` → `player`.
fn exe_stem(exe: &str) -> String {
    let exe = exe.trim();
    let cut = exe.len().checked_sub(4).filter(|&n| exe.is_char_boundary(n) && exe[n..].eq_ignore_ascii_case(".exe"));
    cut.map_or(exe, |n| &exe[..n]).to_owned()
}

#[cfg(test)]
#[path = "record_audio_sources_tests.rs"]
mod tests;
