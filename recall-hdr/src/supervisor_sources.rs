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
    /// The last one queued (or dropped by a full queue) to an attached engine: what it must ack.
    sent: u64,
    /// The last spawn was below the protocol floor (`supervisor_floor.rs`), with whether its game
    /// is captured by process tree: its list (and so the current one) withholds devices.
    below_floor: Option<bool>,
    /// A `hello` without `sync_hold`: every later spawn is below the floor.
    forced_below: bool,
    /// The last attach withheld `sync` (the shape at the attach differed from the live list):
    /// that engine stays held, so it must be respawned (N3), whatever the list does after.
    withheld: bool,
    /// The last attach sent `sync` (the engine records): a later restatement sends one too.
    synced: bool,
    /// An audio line to the attached engine was dropped by a full queue (F-1): the engine holds
    /// on the gap, and the intent is restated until one restatement is queued whole.
    dirty: bool,
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

    /// `line` with the next sequence number, and that number.
    fn stamp(&self, line: &str) -> (String, u64) {
        let mut g = self.lock();
        g.seq += 1;
        (audio_intent::stamped(line, g.seq), g.seq)
    }

    /// What the engine must ack (`status.audio_seq`): the last line actually queued.
    pub(super) fn last_sent(&self) -> u64 {
        self.lock().sent
    }

    /// A line was dropped and no whole restatement has been queued since (F-1).
    pub(super) fn dirty(&self) -> bool {
        self.lock().dirty
    }
}

impl super::Shared {
    /// One audio line to the engine with its sequence number (the caller holds `child_stdin`).
    /// Only a queued line counts as sent (F-1). One a full queue drops leaves a gap in the seqs:
    /// the engine holds every device track on it, and `dirty` makes the controller restate.
    pub(super) fn queue_audio(&self, g: &super::EngineStdin, line: &str) -> Sent {
        let (line, seq) = self.spawn_sources.stamp(line);
        let attached = g.quitting.is_none() && g.queue.is_some();
        let sent = self.queue_line(g, &line);
        if sent.queued() {
            self.spawn_sources.lock().sent = seq;
        } else if attached {
            self.spawn_sources.lock().dirty = true; // the safe state: restate until whole
        }
        sent
    }

    /// After a dropped line: queue a whole restatement now (it may drop again; `dirty` stays).
    pub(super) fn restate_if_dirty(&self, g: &super::EngineStdin) {
        if self.spawn_sources.dirty() && g.quitting.is_none() && g.queue.is_some() {
            self.restate(g, false);
        }
    }

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

    /// The whole audio intent to `queue` (design-audio-mute P8), in this order: the mic mute
    /// (no press), every current source's state, `retire` for each track the spawn was given
    /// that the current list no longer records the same way, then `sync`, which releases the
    /// engine's held tracks. No `sync` when the track shape moved: that engine stays held until
    /// the respawn quits it (B5). The caller holds `child_stdin`. False: a line was not queued.
    pub(super) fn restate(&self, g: &super::EngineStdin, at_attach: bool) -> bool {
        // The engine's ack counts on from this line's seq (F-1).
        let mut lines: Vec<String> = vec![serde_json::json!({"cmd": "restate"}).to_string()];
        if let Some(now) = self.spawn_mute.get() {
            lines.push(crate::record_dispatch::mic_line(now, false)); // never a press (R2-audio-r9-3)
        }
        let (baked, current) = self.spawn_sources.baked_and_current();
        lines.extend(audio_intent::restatement(&baked, &current).iter().map(Value::to_string));
        let same = audio_intent::same_shape(&baked, &current);
        // A running engine (synced at its attach) always gets `sync` again: it releases the hold
        // a gap set. At the attach only when the shape held (else no sync and respawn, N3).
        let sync = same || (!at_attach && self.spawn_sources.lock().synced);
        if sync {
            lines.push(serde_json::json!({"cmd": "sync"}).to_string());
        }
        if at_attach {
            let mut s = self.spawn_sources.lock();
            s.withheld = !same; // no sync: respawn at the pid (N3)
            s.synced = same;
        }
        let all = queue_all(&lines, |l| self.queue_audio(g, l).queued());
        if all {
            self.spawn_sources.lock().dirty = false;
        } else {
            crate::log!("supervisor: the engine's audio restatement could not be queued; its device tracks stay held");
        }
        all
    }
}

/// Queue `lines` in order and stop at the first one not queued (F-1): never a `sync` after a
/// dropped line, which would release the engine's hold without the dropped change.
pub(super) fn queue_all(lines: &[String], mut send: impl FnMut(&str) -> bool) -> bool {
    lines.iter().all(|l| send(l))
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
        let sent = lines.iter().fold(Sent::Queued, |sent, line| sent.then(s.queue_audio(&g, &line.to_string())));
        s.restate_if_dirty(&g); // a line dropped: re-queue the whole intent (F-1)
        sent
    }

    /// The sequence number of the last audio line queued (`status.audio_seq` acks it).
    pub fn last_audio_seq(&self) -> u64 {
        self.shared.spawn_sources.last_sent()
    }

    /// Queue the whole audio intent again (an unacknowledged line, `record_audio_ack`).
    pub fn restate_audio(&self) -> bool {
        let s = &self.shared;
        let g = s.child_stdin.lock().unwrap_or_else(|p| p.into_inner());
        g.quitting.is_none() && g.queue.is_some() && s.restate(&g, false)
    }

    /// An audio line was dropped and no whole restatement is queued since (`record_audio_ack`).
    pub fn audio_dirty(&self) -> bool {
        self.shared.spawn_sources.dirty()
    }
}
