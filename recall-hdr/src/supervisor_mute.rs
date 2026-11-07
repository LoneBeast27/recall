//! The mic mute every engine spawn starts with. The controller's spec carries the mute at the
//! last retarget; a crash respawn reuses that spec, so a mute set since (Alt+M) would be lost
//! until `hello` and the re-send, and the first seconds of the new buffer would hold live mic
//! audio while the tray says muted (2026-11-07 audit R2-deep-audio-1). The controller sets this
//! on every mute change and `engine_command` writes it over the spec's `RECALL_MIC_MUTED`.

use std::sync::atomic::{AtomicU8, Ordering};

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
