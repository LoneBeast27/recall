//! The audio source list every engine spawn starts with (final audit F-audio-1). The spec holds
//! the list as it was at the last retarget or restart; a crash respawn reuses that spec, while a
//! live Audio-page change (`set_source`: a line-in's "Alt+M mutes it", a re-picked twin's id, a
//! gain) reaches only the running engine. Without this a respawn would open the old list: a
//! line-in the user has muted would record, or the twin the user did not pick would open. The
//! controller sets the list on every change, engine running or not, and [`SpawnSources::apply`]
//! writes it over the spec's `RECALL_AUDIO_SOURCES` each time the manager reads the spec. A
//! change during a spawn is restated at the stdin attach before `sync` (design-audio-mute P8);
//! one that moved the track shape gets no `sync` (its engine stays held) and respawns the engine
//! once ([`Shared::respawn_if_sources_moved`]).
//!
//! The cut-down (Rohan 2026-11-09 23:31 UTC): the audio state goes to the engine as one
//! coalescing slot of its whole latest state ([`Shared::publish_audio`],
//! `supervisor_stdin.rs`), never as single lines a full queue could drop, so it needs no
//! sequence numbers, acks or re-sends.

use std::sync::Mutex;

use serde_json::Value;

use crate::audio_intent;
use crate::engine_policy::EngineSpec;
use crate::record_audio_sources::{AudioSource, SOURCES_ENV};
use crate::record_dispatch::Sent;

/// The latest list (`engine_value`), or none before the controller first sets one, with the
/// list the spec the manager last read was given (`baked`).
#[derive(Default)]
pub(super) struct SpawnSources(Mutex<Inner>);

#[derive(Default)]
struct Inner {
    value: Option<String>,
    /// What the last [`SpawnSources::apply`] put in the spec's `RECALL_AUDIO_SOURCES`.
    baked: Option<String>,
    /// The last spawn was below the protocol floor (`supervisor_floor.rs`), with whether its game
    /// is captured by process tree: its list (and so the current one) withholds devices.
    below_floor: Option<bool>,
    /// A `hello` without `sync_hold`: every later spawn is below the floor.
    forced_below: bool,
    /// The last attach withheld `sync` (the shape at the attach differed from the live list):
    /// that engine stays held, so it must be respawned (N3), whatever the list does after.
    withheld: bool,
    /// The last attach sent `sync` (the engine records): every later state sends one too.
    synced: bool,
}

impl SpawnSources {
    pub(super) fn set(&self, value: &str) {
        self.lock().value = Some(value.to_owned());
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.0.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// `spec` with the latest list over its own (unchanged when none was set).
    pub(super) fn apply(&self, spec: Option<EngineSpec>) -> Option<EngineSpec> {
        let mut spec = spec?;
        let mut g = self.lock();
        if let Some(value) = g.value.clone() {
            match spec.env.iter_mut().find(|(k, _)| k == SOURCES_ENV) {
                Some(entry) => entry.1 = Some(value),
                None => spec.env.push((SOURCES_ENV.to_owned(), Some(value))),
            }
        }
        g.baked = spec.env.iter().find(|(k, _)| k == SOURCES_ENV).and_then(|(_, v)| v.clone());
        Some(spec)
    }

    /// (the list the last spawn was given, the current list), as engine lists.
    pub(super) fn baked_and_current(&self) -> (Vec<AudioSource>, Vec<AudioSource>) {
        let g = self.lock();
        let read = |v: &Option<String>| v.as_deref().and_then(audio_intent::parse).unwrap_or_default();
        let current = if g.value.is_some() { read(&g.value) } else { read(&g.baked) };
        let current = match g.below_floor {
            Some(process_loopback) => crate::supervisor::floor::withhold_devices(&current, process_loopback),
            None => current,
        };
        (read(&g.baked), current)
    }

    pub(super) fn set_below_floor(&self, below: Option<bool>) {
        self.lock().below_floor = below;
    }

    pub(super) fn below_floor(&self) -> bool {
        self.lock().below_floor.is_some()
    }

    pub(super) fn force_below(&self) {
        self.lock().forced_below = true;
    }

    pub(super) fn forced_below(&self) -> bool {
        self.lock().forced_below
    }

    /// The list `spec` (as finally spawned) was given.
    pub(super) fn rebake(&self, spec: &EngineSpec) {
        self.lock().baked = spec.env.iter().find(|(k, _)| k == SOURCES_ENV).and_then(|(_, v)| v.clone());
    }

    /// The last attach withheld `sync` (taken once).
    fn take_withheld(&self) -> bool {
        std::mem::take(&mut self.lock().withheld)
    }
}

impl super::Shared {
    /// Right after a spawn shows its pid (review R2-audio-r9-1): when its attach withheld `sync`
    /// (a source change since the spec read moved the track shape), that engine stays held for
    /// good. Retarget generation `gen` to itself, so the manager quits it and respawns on the
    /// current list, even if the list has moved back since (N3). Any other change was restated
    /// at the attach. True when it did.
    pub(super) fn respawn_if_sources_moved(&self, gen: u64) -> bool {
        if !self.spawn_sources.take_withheld() {
            return false;
        }
        let mut g = self.desired.lock().unwrap_or_else(|p| p.into_inner());
        if g.0 != gen || g.1.is_none() {
            return false; // a retarget or stop since: its own spec reads the list
        }
        g.0 += 1;
        g.2 = Sent::Restarting; // a line meanwhile: "Recorder is restarting"
        crate::log!("supervisor: audio sources changed during the spawn; restarting the engine on them");
        true
    }

    /// The whole latest audio state (design-audio-mute P8), in this order: the mic mute (no
    /// press), every current source's state, `retire` for each track the spawn was given that
    /// the current list no longer records the same way, then `sync`, which releases the
    /// engine's held tracks. At the attach no `sync` when the track shape moved: that engine
    /// stays held until the respawn quits it (B5, N3); a running engine (synced at its attach)
    /// gets `sync` every time (harmless: it is already recording).
    fn audio_state(&self, at_attach: bool) -> Vec<String> {
        let mut lines: Vec<String> = Vec::new();
        if let Some(now) = self.spawn_mute.get() {
            lines.push(crate::record_dispatch::mic_line(now, false)); // never a press (R2-audio-r9-3)
        }
        let (baked, current) = self.spawn_sources.baked_and_current();
        lines.extend(audio_intent::restatement(&baked, &current).iter().map(Value::to_string));
        let same = audio_intent::same_shape(&baked, &current);
        let mut s = self.spawn_sources.lock();
        if at_attach {
            s.withheld = !same; // no sync: respawn at the pid (N3)
            s.synced = same;
        }
        if same || (!at_attach && s.synced) {
            lines.push(serde_json::json!({"cmd": "sync"}).to_string());
        }
        lines
    }

    /// Put the whole latest audio state in the engine's state slot (`StdinQueue::send_state`):
    /// it replaces any state not yet written, is never dropped by a full queue, and goes ahead
    /// of a queued `quit`. The caller holds `child_stdin`.
    pub(super) fn publish_audio(&self, g: &super::EngineStdin, at_attach: bool) {
        if let Some(queue) = g.queue.as_ref() {
            queue.send_state(self.audio_state(at_attach));
        }
    }
}

impl super::Supervisor {
    /// An Audio-page change (design-audio-mute P10): the list every later spawn starts with, and
    /// the whole new audio state to the running engine (its tightening parts included: a flag,
    /// a re-picked id, a `retire`), under the one `child_stdin` lock the attach also holds, so a
    /// spawn gets the change in its first state or in the next one. Sent before any restart is
    /// decided. Queued, or why the engine does not take lines now (the state still reaches a
    /// quitting engine ahead of its `quit`).
    pub fn update_audio(&self, value: Option<&str>) -> Sent {
        let s = &self.shared;
        let g = s.child_stdin.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(value) = value {
            s.spawn_sources.set(value);
        }
        s.publish_audio(&g, false);
        match (g.quitting, g.queue.is_some()) {
            (Some(refusal), _) => refusal,
            (None, true) => Sent::Queued,
            (None, false) => Sent::NotResponding,
        }
    }
}
