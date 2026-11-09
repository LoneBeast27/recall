//! The mic mute every engine spawn starts with. The controller's spec carries the mute at the
//! last retarget; a crash respawn reuses that spec, so a mute set since (Alt+M) would be lost
//! until `hello` and the re-send, and the first seconds of the new buffer would hold live mic
//! audio while the tray says muted (2026-11-07 audit R2-deep-audio-1). The controller sets this
//! on every mute change and `engine_command` writes it over the spec's `RECALL_MIC_MUTED`.

use std::sync::atomic::{AtomicU8, Ordering};

use crate::record_dispatch::Sent;

/// The engine's env var for its starting mute (`recall` `audio::MIC_MUTED_ENV`).
pub(super) const MIC_MUTED_ENV: &str = "RECALL_MIC_MUTED";

const UNSET: u8 = 0;
const LIVE: u8 = 1;
const MUTED: u8 = 2;

/// The latest mute, or none before the controller first sets one (the spec's value stands).
pub(super) struct SpawnMute(AtomicU8);

impl SpawnMute {
    pub(super) fn new() -> SpawnMute {
        SpawnMute(AtomicU8::new(UNSET))
    }

    pub(super) fn set(&self, muted: bool) {
        self.0.store(if muted { MUTED } else { LIVE }, Ordering::SeqCst);
    }

    pub(super) fn get(&self) -> Option<bool> {
        match self.0.load(Ordering::SeqCst) {
            MUTED => Some(true),
            LIVE => Some(false),
            _ => None,
        }
    }
}

impl super::Shared {
    /// Alt+M: the spawn mute and the `set_mic_mute` line to the live engine, under the one
    /// `child_stdin` lock that [`attach_spawned`](Self::attach_spawned) also holds: a spawn's
    /// attach sees either the old mute and then gets this line, or the new mute (P1-a).
    pub(super) fn set_mute_and_send(&self, muted: bool) -> Sent {
        let g = self.child_stdin.lock().unwrap_or_else(|p| p.into_inner());
        self.spawn_mute.set(muted);
        self.queue_line(&g, &mute_line(muted))
    }

    /// A spawned engine's stdin; `baked` is the mute its environment was given. An Alt+M from
    /// the env read to here had no queue to take its line (P1-a: live mic in the ring while the
    /// tray says muted): when the mute differs now, `set_mic_mute` is the engine's first line.
    pub(super) fn attach_spawned(&self, w: Option<Box<dyn std::io::Write + Send>>, baked: Option<bool>) {
        let queue = w.map(super::stdin::StdinQueue::start);
        let mut g = self.child_stdin.lock().unwrap_or_else(|p| p.into_inner());
        if queue.is_some() {
            g.quitting = None; // a new engine takes lines
        }
        g.queue = queue; // the old queue closes: its writer ends after its current write
        let Some(queue) = g.queue.as_ref() else { return };
        if let Some(now) = self.spawn_mute.get().filter(|&now| baked != Some(now)) {
            if !queue.send(&mute_line(now)) {
                crate::log!("supervisor: the new engine's mic mute could not be queued");
            }
        }
    }
}

/// The engine command that sets its mic mute.
pub(super) fn mute_line(muted: bool) -> String {
    serde_json::json!({"cmd": "set_mic_mute", "muted": muted}).to_string()
}

#[cfg(test)]
#[path = "supervisor_mute_tests.rs"]
mod tests;
