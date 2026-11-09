//! The mic-mute hotkey's state (Alt+M) for every input track, and how many times it changed.
//! A change is a press: `hold` lets an input opened on an unsure sibling unmute only on a press
//! after it opened (A4 ruling, merge gate p9).

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

static MIC_MUTED: AtomicBool = AtomicBool::new(false);
/// Changes of `MIC_MUTED` so far (a set to the value it already has is not one).
static PRESSES: AtomicU64 = AtomicU64::new(0);

/// The controller's mute at engine spawn (`1` = muted).
pub(crate) const MIC_MUTED_ENV: &str = "RECALL_MIC_MUTED";

/// The mute a new engine starts with, from its environment (`get`): the controller passes its
/// state at spawn, so a muted mic is silent from the run's first sample (the `set_mic_mute`
/// after `hello` comes after every track has started recording).
pub(crate) fn initial_mute(get: impl Fn(&str) -> Option<String>) -> bool {
    get(MIC_MUTED_ENV).is_some_and(|v| v.trim() == "1")
}

/// Set whether newly captured input samples enter the replay buffer as silence.
pub fn set_mic_muted(muted: bool) {
    if MIC_MUTED.swap(muted, Ordering::SeqCst) != muted {
        PRESSES.fetch_add(1, Ordering::SeqCst);
    }
}

/// Toggle microphone capture muting and return the new state.
pub fn toggle_mic_mute() -> bool {
    let muted = MIC_MUTED.fetch_xor(true, Ordering::SeqCst) ^ true;
    PRESSES.fetch_add(1, Ordering::SeqCst);
    muted
}

/// Return whether newly captured input samples are muted.
pub fn mic_muted() -> bool {
    MIC_MUTED.load(Ordering::SeqCst)
}

/// The hotkey's state as a packet is stored: muted, and the presses so far.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct MuteNow {
    pub muted: bool,
    pub presses: u64,
}

impl MuteNow {
    /// The engine's state right now (read by `TrackRing::push_live` for every packet).
    pub(crate) fn read() -> Self {
        // Presses first: a press between the two reads is then seen as muted or counted later.
        let presses = PRESSES.load(Ordering::SeqCst);
        MuteNow { muted: mic_muted(), presses }
    }
}
