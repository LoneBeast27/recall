//! The engine slot (`Local\Recall.Engine`) held by another recorder: an orphan of a
//! crashed controller finishing its save, a hand-run `recall.exe`, a dev script. The
//! engine exits [`crate::engine_policy::EXIT_SLOT_BUSY`] when it cannot take the slot;
//! the supervisor retries (not a failure) for up to [`SLOT_HELD_GIVE_UP`], then gives up.
//! Meanwhile the tray says [`OTHER_RECORDER`] instead of "Starting capture", the busy
//! retries are logged once per [`SLOT_LOG_EVERY`], and the give-up shows the same text
//! on the island.

use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU8, Ordering};
use std::time::{Duration, Instant};

use crate::record_notice::Situation;

/// How long the slot may stay held before the supervisor gives up: the orphan's own save
/// cap (`recall` `ORPHAN_SAVE_BUDGET`, 30 min) plus 2 min for its exit.
pub const SLOT_HELD_GIVE_UP: Duration = Duration::from_secs(32 * 60);
/// Busy this long before the tray says so (a normal restart's last 0.5 s stays quiet).
pub const SLOT_HELD_SHOW: Duration = Duration::from_secs(10);
/// The busy retries are logged at most this often.
pub const SLOT_LOG_EVERY: Duration = Duration::from_secs(60);
/// The tray line while the slot is held, and the island's title once the supervisor gives up.
pub const OTHER_RECORDER: &str = "Another Recall recorder is still running";
/// The island's detail on that give-up.
pub const OTHER_RECORDER_DETAIL: &str = "Not recording (see recall-hdr.log)";

/// Whether another recorder holds the engine slot, as the supervisor last saw it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SlotHeld {
    Free,
    /// The busy retries go on.
    Held,
    /// The supervisor gave up after [`SLOT_HELD_GIVE_UP`] (until the next target).
    GaveUp,
}

/// The supervisor's slot state, shared with the controller. An engine that said `hello`
/// after the mark took the slot, so the mark reads as free from then on.
#[derive(Debug, Default)]
pub struct SlotFlag {
    state: AtomicU8,
    hello_mark: AtomicU32,
    /// This busy retry is not logged (the engine's own [`ENGINE_BUSY_LINE`] neither).
    quiet: AtomicBool,
}

/// The engine's log line when the slot is held (`recall/src/main.rs` `claim_recorder`).
pub const ENGINE_BUSY_LINE: &str = "another Recall engine is already recording";

impl SlotFlag {
    pub fn set(&self, held: SlotHeld, hellos: u32) {
        self.hello_mark.store(hellos, Ordering::SeqCst);
        let v = match held {
            SlotHeld::Free => 0,
            SlotHeld::Held => 1,
            SlotHeld::GaveUp => 2,
        };
        self.state.store(v, Ordering::SeqCst);
    }

    pub fn set_quiet(&self, quiet: bool) {
        self.quiet.store(quiet, Ordering::SeqCst);
    }

    /// The engine's `message` is its busy line during a quiet retry: not logged.
    pub fn mutes(&self, message: &str) -> bool {
        self.quiet.load(Ordering::SeqCst) && message.contains(ENGINE_BUSY_LINE)
    }

    /// The state now, given the engine hellos seen so far.
    pub fn get(&self, hellos: u32) -> SlotHeld {
        if hellos > self.hello_mark.load(Ordering::SeqCst) {
            return SlotHeld::Free;
        }
        match self.state.load(Ordering::SeqCst) {
            1 => SlotHeld::Held,
            2 => SlotHeld::GaveUp,
            _ => SlotHeld::Free,
        }
    }
}

/// A rate limit for the busy-retry log lines.
#[derive(Debug, Default)]
pub struct LogEvery {
    last: Option<Instant>,
}

impl LogEvery {
    /// After a busy retry: whether the next retry stays quiet. A loud retry (it logged)
    /// starts the minute; a quiet one turns loud once the minute is over.
    pub fn next_quiet(&mut self, was_loud: bool, now: Instant) -> bool {
        if was_loud {
            self.last = Some(now);
            return true;
        }
        self.last.is_some_and(|t| now.saturating_duration_since(t) < SLOT_LOG_EVERY)
    }

    /// The busy phase ended: the next busy retry logs at once.
    pub fn clear(&mut self) {
        self.last = None;
    }
}

/// The tray line for `state` while the slot is held (None: the normal line).
pub fn slot_tray_line(state: &str, slot: SlotHeld) -> Option<&'static str> {
    (slot != SlotHeld::Free && matches!(state, "starting" | "error")).then_some(OTHER_RECORDER)
}

/// The island situation for `state` once the supervisor gave up on the slot.
pub fn slot_situation(state: &str, slot: SlotHeld) -> Option<Situation> {
    (slot == SlotHeld::GaveUp && state == "error").then_some(Situation::OtherRecorder)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_hello_after_the_mark_frees_the_slot() {
        let f = SlotFlag::default();
        assert_eq!(f.get(0), SlotHeld::Free);
        f.set(SlotHeld::Held, 4);
        assert_eq!(f.get(4), SlotHeld::Held);
        assert_eq!(f.get(5), SlotHeld::Free, "the next engine took the slot");
        f.set(SlotHeld::GaveUp, 5);
        assert_eq!(f.get(5), SlotHeld::GaveUp);
        f.set(SlotHeld::Free, 5);
        assert_eq!(f.get(5), SlotHeld::Free);
        let busy = "another Recall engine is already recording; exiting";
        assert!(!f.mutes(busy));
        f.set_quiet(true);
        assert!(f.mutes(busy) && !f.mutes("encoder opened"), "only the busy line is muted");
    }

    #[test]
    fn busy_retries_log_once_a_minute() {
        // A retry every 8 s for 5 min: the first logs, then one per minute.
        let t0 = Instant::now();
        let mut log = LogEvery::default();
        let mut quiet = false;
        let mut loud = 0;
        for i in 0..38u64 {
            if !quiet {
                loud += 1;
            }
            quiet = log.next_quiet(!quiet, t0 + Duration::from_secs(i * 8));
        }
        assert!((5..=6).contains(&loud), "{loud} logged retries in 5 min");
        log.clear();
        assert!(!log.next_quiet(false, t0 + Duration::from_secs(400)), "a new busy phase logs at once");
    }

    #[test]
    fn the_tray_and_island_say_another_recorder_holds_the_slot() {
        assert_eq!(slot_tray_line("starting", SlotHeld::Held), Some(OTHER_RECORDER));
        assert_eq!(slot_tray_line("error", SlotHeld::GaveUp), Some(OTHER_RECORDER));
        assert_eq!(slot_tray_line("starting", SlotHeld::Free), None);
        assert_eq!(slot_tray_line("recording", SlotHeld::Held), None, "a recording engine owns the slot");
        assert_eq!(slot_situation("error", SlotHeld::GaveUp), Some(Situation::OtherRecorder));
        assert_eq!(slot_situation("error", SlotHeld::Held), None);
        assert_eq!(Situation::OtherRecorder.title(), OTHER_RECORDER);
        assert!(Situation::OtherRecorder.is_failure());
    }
}
