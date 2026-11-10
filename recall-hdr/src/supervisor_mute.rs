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
        let sent = self.queue_line(&g, &mute_line(muted)); // the press (A4 adopts only on one)
        self.publish_audio(&g, false); // the state itself: never dropped (the cut-down)
        sent
    }

    /// A spawned engine's stdin. Its device tracks start held: the whole audio intent is
    /// restated as its first lines, then `sync` (`publish_audio`, design-audio-mute P8), all under
    /// the `child_stdin` lock an Alt+M and an Audio-page change also hold, so none of them is
    /// lost between the spawn's env and here. `_baked`: the mute its env was given (the
    /// restatement sends the current one whatever it was).
    pub(super) fn attach_spawned(&self, w: Option<Box<dyn std::io::Write + Send>>, _baked: Option<bool>) {
        let queue = w.map(super::stdin::StdinQueue::start);
        let mut g = self.child_stdin.lock().unwrap_or_else(|p| p.into_inner());
        if queue.is_some() {
            g.quitting = None; // a new engine takes lines
        }
        g.queue = queue; // the old queue closes: its writer ends after its current write
        self.publish_audio(&g, true);
    }
}

/// The engine command for the user's Alt+M (`set_mic` through `set_mute_and_send`): a press
/// (gate review P2-b). A resync and the catch-up at attach send no press.
pub(super) fn mute_line(muted: bool) -> String {
    crate::record_dispatch::mic_line(muted, true)
}

#[cfg(test)]
#[path = "supervisor_mute_tests.rs"]
mod tests;
