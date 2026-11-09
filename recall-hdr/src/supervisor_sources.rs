//! The audio source list every engine spawn starts with (final audit F-audio-1). The spec holds
//! the list as it was at the last retarget or restart; a crash respawn reuses that spec, while a
//! live Audio-page change (`set_source`: a line-in's "Alt+M mutes it", a re-picked twin's id, a
//! gain) reaches only the running engine. Without this a respawn would open the old list: a
//! line-in the user has muted would record, or the twin the user did not pick would open. The
//! controller sets the list on every change, engine running or not, and [`SpawnSources::apply`]
//! writes it over the spec's `RECALL_AUDIO_SOURCES` each time the manager reads the spec.

use std::sync::Mutex;

use crate::engine_policy::EngineSpec;
use crate::record_audio_sources::SOURCES_ENV;

/// The latest list (`engine_value`), or none before the controller first sets one.
#[derive(Default)]
pub(super) struct SpawnSources(Mutex<Option<String>>);

impl SpawnSources {
    pub(super) fn set(&self, value: &str) {
        *self.0.lock().unwrap_or_else(|p| p.into_inner()) = Some(value.to_owned());
    }

    /// `spec` with the latest list over its own (unchanged when none was set).
    pub(super) fn apply(&self, spec: Option<EngineSpec>) -> Option<EngineSpec> {
        let mut spec = spec?;
        let Some(value) = self.0.lock().unwrap_or_else(|p| p.into_inner()).clone() else { return Some(spec) };
        match spec.env.iter_mut().find(|(k, _)| k == SOURCES_ENV) {
            Some(entry) => entry.1 = Some(value),
            None => spec.env.push((SOURCES_ENV.to_owned(), Some(value))),
        }
        Some(spec)
    }
}
