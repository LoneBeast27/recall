//! The recorded sources: `RECALL_AUDIO_SOURCES` (the controller's `recording.audio_sources`,
//! enabled ones, as JSON) in track order. Each source is one track named after it:
//! - `game`: the game's process tree (`RECALL_AUDIO_GAME_PID`); on the desktop, loopback
//!   of `device` ("" = the Windows default output).
//! - `output`: loopback of an output device; `input`: a microphone or line-in.
//! - `app`: an app's process tree by `exe` (`apps.rs`).
//!
//! Without `RECALL_AUDIO_SOURCES` (an engine started by hand or by an older controller)
//! the old variables still work: Game (`RECALL_AUDIO_GAME`), Mic (`RECALL_AUDIO_MIC`, none
//! with `RECALL_AUDIO_MIC_OFF=1`), a voice app (`RECALL_AUDIO_VOICE_EXE`) and
//! `RECALL_AUDIO_APPS`.

use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};

use super::apps::{self, AppSource};
use super::TrackRing;

pub const SOURCES_ENV: &str = "RECALL_AUDIO_SOURCES";
/// Same cap as the controller.
pub const SOURCES_MAX: usize = 8;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Game,
    Output,
    Input,
    App,
}

/// One source as the controller sends it.
#[derive(Clone, Debug, PartialEq, Deserialize)]
pub struct SourceSpec {
    pub id: String,
    pub kind: String,
    #[serde(default)]
    pub device: String,
    /// The endpoint id (`IMMDevice::GetId`) the Audio page saved with `device` when it was
    /// picked; "" = none (a config written before the id, or a hand-made one), and so is a
    /// malformed one (`endpoints::id_ok`). Only decides whether a line-in may skip Alt+M (only
    /// with an id, `endpoints::opened_own`), never which endpoint opens.
    #[serde(default)]
    pub device_id: String,
    #[serde(default)]
    pub exe: String,
    pub name: String,
    #[serde(default)]
    pub gain_db: f32,
    /// Whether the mic-mute hotkey silences this input: `true` a microphone, `false` a line-in
    /// or capture card; absent = muted (every config written before the flag, the default mic).
    /// Ignored for other kinds.
    #[serde(default)]
    pub mic_mute: Option<bool>,
}

impl SourceSpec {
    pub fn kind(&self) -> Option<Kind> {
        Some(match self.kind.as_str() {
            "game" => Kind::Game,
            "output" => Kind::Output,
            "input" => Kind::Input,
            "app" => Kind::App,
            _ => return None,
        })
    }

    /// Linear factor for `gain_db`, clamped to the controller's -60..+12 dB.
    pub fn gain(&self) -> f32 {
        gain_of(self.gain_db)
    }
}

/// Linear factor for a gain in dB, clamped to the controller's -60..+12 dB (NaN = 0 dB).
pub fn gain_of(gain_db: f32) -> f32 {
    let db = if gain_db.is_finite() { gain_db.clamp(-60.0, 12.0) } else { 0.0 };
    10f32.powf(db / 20.0)
}

fn spec(id: &str, kind: &str, name: &str, device: &str, exe: &str) -> SourceSpec {
    SourceSpec { device_id: String::new(), id: id.into(), kind: kind.into(), device: device.trim().into(), exe: exe.trim().into(), name: name.into(), gain_db: 0.0, mic_mute: None }
}

/// The sources in `json`, with every bad entry skipped and logged: one broken entry (a
/// duplicate id from a copy-paste, a typo in its kind, a ninth source) must not take the other
/// tracks, the game's included, out of every clip. Only unreadable JSON is an error.
pub fn parse(json: &str) -> Result<Vec<SourceSpec>, String> {
    let (list, skipped) = parse_entries(json)?;
    for why in skipped {
        eprintln!("[recall] {SOURCES_ENV}: skipped {why}");
    }
    Ok(list)
}

/// [`parse`]'s list and why each skipped entry was skipped.
pub(super) fn parse_entries(json: &str) -> Result<(Vec<SourceSpec>, Vec<String>), String> {
    let entries: Vec<serde_json::Value> = serde_json::from_str(json).map_err(|e| format!("{SOURCES_ENV}: {e}"))?;
    let mut ids = std::collections::HashSet::new();
    let (mut list, mut skipped) = (Vec::new(), Vec::new());
    for (n, entry) in entries.into_iter().enumerate() {
        match entry_problem(serde_json::from_value::<SourceSpec>(entry), &mut ids) {
            Ok(s) if list.len() >= SOURCES_MAX => skipped.push(format!("source '{}': at most {SOURCES_MAX} sources", s.id)),
            Ok(s) => list.push(s),
            Err(why) => skipped.push(format!("entry {}: {why}", n + 1)),
        }
    }
    Ok((list, skipped))
}

/// One entry, or what is wrong with it; `ids` holds the ids taken by earlier entries.
fn entry_problem(entry: Result<SourceSpec, serde_json::Error>, ids: &mut std::collections::HashSet<String>) -> Result<SourceSpec, String> {
    let s = entry.map_err(|e| e.to_string())?;
    if s.id.trim().is_empty() || ids.contains(&s.id.trim().to_lowercase()) {
        return Err(format!("source id '{}' is empty or used twice", s.id));
    }
    if s.kind().is_none() {
        return Err(format!("source '{}' has unknown kind '{}'", s.id, s.kind));
    }
    if s.kind() == Some(Kind::App) && (s.exe.trim().is_empty() || s.exe.contains(['\\', '/', ':'])) {
        return Err(format!("source '{}' needs an exe file name", s.id));
    }
    ids.insert(s.id.trim().to_lowercase());
    Ok(s)
}

/// The sources for this engine run (`get` reads the environment; injected in tests).
pub fn resolve(get: impl Fn(&str) -> Option<String>) -> Result<Vec<SourceSpec>, String> {
    if let Some(json) = get(SOURCES_ENV) {
        return parse(&json);
    }
    let set = |key: &str| get(key).filter(|v| !v.trim().is_empty());
    let mut list = vec![spec("game", "game", "Game", &set("RECALL_AUDIO_GAME").unwrap_or_default(), "")];
    if set("RECALL_AUDIO_MIC_OFF").as_deref().map(str::trim) != Some("1") {
        list.push(spec("mic", "input", "Microphone", &set("RECALL_AUDIO_MIC").unwrap_or_default(), ""));
    }
    if let Some(exe) = set("RECALL_AUDIO_VOICE_EXE") {
        list.push(spec("voice", "app", "Voice chat", "", &exe));
    }
    if let Some(json) = set(apps::APPS_ENV) {
        for (n, app) in apps::parse(&json)?.into_iter().enumerate() {
            list.push(spec(&format!("app-{}", n + 1), "app", &app.track_name(), "", &app.exe));
        }
    }
    list.truncate(SOURCES_MAX);
    Ok(list)
}

pub fn from_env() -> Result<Vec<SourceSpec>, String> {
    resolve(|key| std::env::var(key).ok())
}

/// A change the Audio page makes to a running source: gain, name, mic-mute flag, and the
/// endpoint id saved with its device (P1-B; the track reopens on it).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct LiveChange<'a> {
    pub gain_db: Option<f32>,
    pub name: Option<&'a str>,
    pub mic_mute: Option<bool>,
    pub device_id: Option<&'a str>,
}

/// Apply `change` to the live track (no engine restart, so the replay buffer stays). False
/// when no live track has that id.
pub fn set_live(id: &str, change: LiveChange) -> bool {
    let LiveChange { gain_db, name, mic_mute, device_id } = change;
    let Ok(mut states) = STATES.lock() else { return false };
    let mut found = false;
    for (sid, sname, state) in states.iter_mut() {
        if sid.trim() != id.trim() {
            continue;
        }
        if let Some(name) = name.map(str::trim).filter(|n| !n.is_empty()) {
            *sname = name.to_owned();
            if let State::Live(ring) = state {
                ring.set_title(name);
            }
        }
        if let (Some(db), State::Live(ring)) = (gain_db, &*state) {
            ring.set_gain_db(db);
        }
        if let (Some(on), State::Live(ring)) = (mic_mute, &*state) {
            ring.set_mic_mute(on);
        }
        if let (Some(device_id), State::Live(ring)) = (device_id, &*state) {
            // A malformed id counts as none (`endpoints::id_ok`), as at spawn.
            ring.live.set_saved_id(if super::endpoints::id_ok(device_id) { device_id } else { "" });
        }
        found |= matches!(state, State::Live(_));
    }
    found
}

/// What one source opened, for the engine status (`audio` in `status`).
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Opened {
    pub id: String,
    pub name: String,
    /// The endpoint or process captured ("… (Windows default)" for a source with no device,
    /// "<name> (not connected; waiting)" for a named device not there yet), or why there is no
    /// track.
    pub opened: String,
    pub live: bool,
    /// The endpoint id an unsure sibling offers as the source's own (`hold.rs`).
    pub adopt_id: Option<String>,
}

enum State {
    Live(Arc<TrackRing>),
    Failed(String),
}

static STATES: Mutex<Vec<(String, String, State)>> = Mutex::new(Vec::new());

pub(super) fn note_live(spec: &SourceSpec, ring: &Arc<TrackRing>) {
    if let Ok(mut states) = STATES.lock() {
        states.push((spec.id.clone(), spec.name.clone(), State::Live(Arc::clone(ring))));
    }
}

pub(super) fn note_failed(spec: &SourceSpec, why: String) {
    if let Ok(mut states) = STATES.lock() {
        states.push((spec.id.clone(), spec.name.clone(), State::Failed(why)));
    }
}

/// Every source's current capture, in list order.
pub fn opened() -> Vec<Opened> {
    let Ok(states) = STATES.lock() else { return Vec::new() };
    states
        .iter()
        .map(|(id, name, state)| match state {
            State::Live(ring) => Opened { id: id.clone(), name: name.clone(), opened: ring.endpoint(), live: true, adopt_id: ring.live.adopted() },
            State::Failed(why) => Opened { id: id.clone(), name: name.clone(), opened: why.clone(), live: false, adopt_id: None },
        })
        .collect()
}

/// The app sources as `apps.rs` takes them (track name, ring id and gain from the source).
pub(super) fn app_sources(list: &[SourceSpec]) -> Vec<AppSource> {
    list.iter()
        .filter(|s| s.kind() == Some(Kind::App))
        .map(|s| AppSource { exe: s.exe.trim().to_owned(), label: s.name.clone(), id: s.id.clone(), gain_db: s.gain_db })
        .collect()
}

/// `rings` in the order of `list` (app tracks start together, after the others).
pub(super) fn in_list_order(list: &[SourceSpec], mut rings: Vec<Arc<TrackRing>>) -> Vec<Arc<TrackRing>> {
    rings.sort_by_key(|r| list.iter().position(|s| s.id.trim() == r.name).unwrap_or(usize::MAX));
    rings
}

#[cfg(test)]
#[path = "sources_tests.rs"]
mod tests;
