//! The audio source list every engine spawn starts with (final audit F-audio-1). The spec holds
//! the list as it was at the last retarget or restart; a crash respawn reuses that spec, while a
//! live Audio-page change (`set_source`: a line-in's "Alt+M mutes it", a re-picked twin's id, a
//! gain) reaches only the running engine. Without this a respawn would open the old list: a
//! line-in the user has muted would record, or the twin the user did not pick would open. The
//! controller sets the list on every change, engine running or not, and [`SpawnSources::apply`]
//! writes it over the spec's `RECALL_AUDIO_SOURCES` each time the manager reads the spec. A
//! change during a spawn is restated at the stdin attach before `sync` (design-audio-mute P8);
//! one that moved the track shape gets no `sync` (its engine stays held) and respawns the engine
//! once ([`Shared::respawn_if_sources_moved`]). Every audio line carries a sequence number.

use std::sync::Mutex;

use serde_json::Value;

use crate::audio_intent;
use crate::engine_policy::EngineSpec;
use crate::record_audio_sources::{AudioSource, SOURCES_ENV};
use crate::record_dispatch::Sent;

/// The latest list (`engine_value`), or none before the controller first sets one, with the
/// list the spec the manager last read was given (`baked`), and the audio line counter.
#[derive(Default)]
pub(super) struct SpawnSources(Mutex<Inner>);

#[derive(Default)]
struct Inner {
    value: Option<String>,
    /// What the last [`SpawnSources::apply`] put in the spec's `RECALL_AUDIO_SOURCES`.
    baked: Option<String>,
    /// The last sequence number given to an audio line.
    seq: u64,
    /// The last spawn was below the protocol floor (`supervisor_floor.rs`), with whether its game
    /// is captured by process tree: its list (and so the current one) withholds devices.
    below_floor: Option<bool>,
    /// A `hello` without `sync_hold`: every later spawn is below the floor.
    forced_below: bool,
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

    /// The current list records other tracks than the last spawn was given (a restart).
    fn shape_moved(&self) -> bool {
        let (baked, current) = self.baked_and_current();
        !audio_intent::same_shape(&baked, &current)
    }

    /// `line` with the next sequence number.
    pub(super) fn stamp(&self, line: &str) -> String {
        let mut g = self.lock();
        g.seq += 1;
        audio_intent::stamped(line, g.seq)
    }

    pub(super) fn last_seq(&self) -> u64 {
        self.lock().seq
    }
}

impl super::Shared {
    /// Right after a spawn shows its pid (review R2-audio-r9-1): a source change since the spec
    /// read that moved the track shape got no `sync` at the attach, so that engine stays held.
    /// Retarget generation `gen` to itself, so the manager quits it and respawns on the current
    /// list. Any other change was restated at the attach. True when it did.
    pub(super) fn respawn_if_sources_moved(&self, gen: u64) -> bool {
        if !self.spawn_sources.shape_moved() {
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

    /// The whole audio intent to `queue` (design-audio-mute P8), in this order: the mic mute
    /// (no press), every current source's state, `retire` for each track the spawn was given
    /// that the current list no longer records the same way, then `sync`, which releases the
    /// engine's held tracks. No `sync` when the track shape moved: that engine stays held until
    /// the respawn quits it (B5). The caller holds `child_stdin`. False: a line was not queued.
    pub(super) fn restate(&self, queue: &super::stdin::StdinQueue) -> bool {
        let mut lines: Vec<String> = Vec::new();
        if let Some(now) = self.spawn_mute.get() {
            lines.push(crate::record_dispatch::mic_line(now, false)); // never a press (R2-audio-r9-3)
        }
        let (baked, current) = self.spawn_sources.baked_and_current();
        lines.extend(audio_intent::restatement(&baked, &current).iter().map(Value::to_string));
        if audio_intent::same_shape(&baked, &current) {
            lines.push(serde_json::json!({"cmd": "sync"}).to_string());
        }
        let all = lines.iter().all(|l| queue.send(&self.spawn_sources.stamp(l)));
        if !all {
            crate::log!("supervisor: the engine's audio restatement could not be queued; its device tracks stay held");
        }
        all
    }
}

impl super::Supervisor {
    /// An Audio-page change (design-audio-mute P10): the list every later spawn starts with, and
    /// its live lines (`audio_intent::change_lines`) to the running engine, under the one
    /// `child_stdin` lock the attach restatement holds, so a spawn gets either the change in
    /// its restatement or these lines after it. Sent before any restart is decided.
    pub fn update_audio(&self, value: Option<&str>, lines: &[Value]) -> Sent {
        let s = &self.shared;
        let g = s.child_stdin.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(value) = value {
            s.spawn_sources.set(value);
        }
        // Below the protocol floor the engine was given no input or picked device: none is sent.
        let kept: Vec<String> = s.spawn_sources.baked_and_current().1.into_iter().map(|c| c.id).collect();
        let below = s.spawn_sources.below_floor();
        let lines: Vec<&Value> = lines.iter().filter(|l| !below || l["id"].as_str().is_some_and(|id| kept.iter().any(|k| k.trim() == id.trim()))).collect();
        lines.iter().fold(Sent::Queued, |sent, line| sent.then(s.queue_line(&g, &s.spawn_sources.stamp(&line.to_string()))))
    }

    /// The sequence number of the last audio line queued (`status.audio_seq` acks it).
    pub fn last_audio_seq(&self) -> u64 {
        self.shared.spawn_sources.last_seq()
    }

    /// Queue the whole audio intent again (an unacknowledged line, `record_audio_ack`).
    pub fn restate_audio(&self) -> bool {
        let s = &self.shared;
        let g = s.child_stdin.lock().unwrap_or_else(|p| p.into_inner());
        g.quitting.is_none() && g.queue.as_ref().is_some_and(|q| s.restate(q))
    }
}
