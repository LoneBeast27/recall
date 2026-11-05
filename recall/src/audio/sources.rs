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
    #[serde(default)]
    pub exe: String,
    pub name: String,
    #[serde(default)]
    pub gain_db: f32,
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
        10f32.powf(self.gain_db.clamp(-60.0, 12.0) / 20.0)
    }
}

fn spec(id: &str, kind: &str, name: &str, device: &str, exe: &str) -> SourceSpec {
    SourceSpec { id: id.into(), kind: kind.into(), device: device.trim().into(), exe: exe.trim().into(), name: name.into(), gain_db: 0.0 }
}

pub fn parse(json: &str) -> Result<Vec<SourceSpec>, String> {
    let list: Vec<SourceSpec> = serde_json::from_str(json).map_err(|e| format!("{SOURCES_ENV}: {e}"))?;
    if list.len() > SOURCES_MAX {
        return Err(format!("{SOURCES_ENV} lists {} sources; at most {SOURCES_MAX}", list.len()));
    }
    let mut ids = std::collections::HashSet::new();
    for s in &list {
        if s.id.trim().is_empty() || !ids.insert(s.id.trim().to_lowercase()) {
            return Err(format!("{SOURCES_ENV}: source id '{}' is empty or used twice", s.id));
        }
        if s.kind().is_none() {
            return Err(format!("{SOURCES_ENV}: source '{}' has unknown kind '{}'", s.id, s.kind));
        }
        if s.kind() == Some(Kind::App) && (s.exe.trim().is_empty() || s.exe.contains(['\\', '/', ':'])) {
            return Err(format!("{SOURCES_ENV}: source '{}' needs an exe file name", s.id));
        }
    }
    Ok(list)
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

/// What one source opened, for the engine status (`audio` in `status`).
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Opened {
    pub id: String,
    pub name: String,
    /// The endpoint or process captured ("… (Windows default)" when it fell back), or why
    /// there is no track.
    pub opened: String,
    pub live: bool,
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
            State::Live(ring) => Opened { id: id.clone(), name: name.clone(), opened: ring.endpoint(), live: true },
            State::Failed(why) => Opened { id: id.clone(), name: name.clone(), opened: why.clone(), live: false },
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
    rings.sort_by_key(|r| list.iter().position(|s| s.id == r.name).unwrap_or(usize::MAX));
    rings
}

#[cfg(test)]
#[path = "sources_tests.rs"]
mod tests;
