//! The audio source list every engine spawn starts with (final audit F-audio-1). The spec holds
//! the list as it was at the last retarget or restart; a crash respawn reuses that spec, while a
//! live Audio-page change (`set_source`: a line-in's "Alt+M mutes it", a re-picked twin's id, a
//! gain) reaches only the running engine. Without this a respawn would open the old list: a
//! line-in the user has muted would record, or the twin the user did not pick would open. The
//! controller sets the list on every change, engine running or not, and [`SpawnSources::apply`]
//! writes it over the spec's `RECALL_AUDIO_SOURCES` each time the manager reads the spec. A
//! change during a spawn (after the read, before the pid) respawns the engine once
//! ([`Shared::respawn_if_sources_moved`]).

use std::sync::Mutex;

use crate::engine_policy::EngineSpec;
use crate::record_audio_sources::SOURCES_ENV;

/// The latest list (`engine_value`), or none before the controller first sets one, with a
/// version: every set bumps it, and the spec the manager last read notes the one it carried.
#[derive(Default)]
pub(super) struct SpawnSources(Mutex<Inner>);

#[derive(Default)]
struct Inner {
    value: Option<String>,
    version: u64,
    /// The version the last [`SpawnSources::apply`] baked into a spec.
    baked: u64,
}

impl SpawnSources {
    pub(super) fn set(&self, value: &str) {
        let mut g = self.0.lock().unwrap_or_else(|p| p.into_inner());
        g.value = Some(value.to_owned());
        g.version += 1;
    }

    /// `spec` with the latest list over its own (unchanged when none was set).
    pub(super) fn apply(&self, spec: Option<EngineSpec>) -> Option<EngineSpec> {
        let mut spec = spec?;
        let mut g = self.0.lock().unwrap_or_else(|p| p.into_inner());
        g.baked = g.version;
        let Some(value) = g.value.clone() else { return Some(spec) };
        match spec.env.iter_mut().find(|(k, _)| k == SOURCES_ENV) {
            Some(entry) => entry.1 = Some(value),
            None => spec.env.push((SOURCES_ENV.to_owned(), Some(value))),
        }
        Some(spec)
    }

    /// The list changed since the last spec was read (a change during that spawn).
    fn moved(&self) -> bool {
        let g = self.0.lock().unwrap_or_else(|p| p.into_inner());
        g.version != g.baked
    }
}

impl super::Shared {
    /// Right after a spawn shows its pid (review R2-audio-r9-1): a source change that landed
    /// between the spec read and here reached neither the env nor the engine (the controller
    /// sends `set_source` only to a running engine). Retarget generation `gen` to itself, so
    /// the manager quits this engine and respawns it on the current list. A change after this
    /// point is sent live. True when it did.
    pub(super) fn respawn_if_sources_moved(&self, gen: u64) -> bool {
        if !self.spawn_sources.moved() {
            return false;
        }
        let mut g = self.desired.lock().unwrap_or_else(|p| p.into_inner());
        if g.0 != gen || g.1.is_none() {
            return false; // a retarget or stop since: its own spec reads the list
        }
        g.0 += 1;
        g.2 = crate::record_dispatch::Sent::Restarting; // a line meanwhile: "Recorder is restarting"
        crate::log!("supervisor: audio sources changed during the spawn; restarting the engine on them");
        true
    }
}
