//! The engine's `status.save_writing` (`{"bytes":N,"queued":M}`: the `.part` of the
//! save being written and the saves behind it) as the controller follows it. Bytes
//! that change show the save is still being written, so the queue's window moves on
//! and every restart kind waits; bytes stuck for [`SAVE_STALE`] are a wedged writer
//! and expire as before. An engine without the field reports nothing here, so only
//! the plain window applies (today's behaviour).
//!
//! Also here: answers that arrive for a queue that already expired. They drain that
//! queue's count, never the queue sent since (an expired save's late `saved` would
//! otherwise let a restart kill the newer saves).

use std::time::Instant;

use serde_json::Value;

use super::{EngineActivity, SAVE_STALE};

/// `status.save_writing.bytes`, or `None` when the line reports no save in the saver
/// (an engine without the field never reports one).
pub fn writing_bytes(ev: &Value) -> Option<u64> {
    let w = ev.get("save_writing")?;
    Some(w.get("bytes").and_then(Value::as_u64).unwrap_or(0))
}

/// The last `save_writing` seen and when its bytes last changed.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct WriteWatch {
    last: Option<u64>,
    moved: Option<Instant>,
}

impl WriteWatch {
    /// One status line at `now`. True when the save moved on: a save appeared, or
    /// its bytes changed (a new save in front starts small again). A press that
    /// only grows the queue is not progress.
    pub fn status(&mut self, ev: &Value, now: Instant) -> bool {
        let seen = writing_bytes(ev);
        let moved = seen.is_some() && seen != self.last;
        if moved {
            self.moved = Some(now);
        }
        self.last = seen;
        moved
    }

    /// The engine reports a save whose bytes moved within [`SAVE_STALE`].
    pub fn writing(&self, now: Instant) -> bool {
        self.last.is_some() && self.moved.is_some_and(|t| now.duration_since(t) < SAVE_STALE)
    }
}

impl EngineActivity {
    /// A `status` line: a save whose bytes grow keeps its queue's window open.
    pub(super) fn on_status(&mut self, ev: &Value, now: Instant) {
        if self.write.status(ev, now) && self.pending_saves > 0 {
            self.save_progress = Some(now);
        }
    }

    /// A `saved` / `save_failed` for the oldest save. A rejection is the newest
    /// save's (it failed as it was sent), so it is the current queue's; any other
    /// answer first drains a queue that expired unanswered.
    pub(super) fn answered(&mut self, ev: &Value) {
        let rejected = ev.get("rejected").and_then(Value::as_bool) == Some(true);
        if self.expired_saves > 0 && (!rejected || self.pending_saves == 0) {
            self.expired_saves -= 1;
        } else {
            self.pending_saves = self.pending_saves.saturating_sub(1);
        }
    }

    /// Saves the engine is still writing: counted, or reported by the engine with
    /// bytes still moving. A restart now aborts them.
    pub fn saves_writing(&mut self, now: Instant) -> bool {
        self.expire_saves(now);
        self.pending_saves > 0 || self.write.writing(now)
    }

    /// The PC woke up: the clock ran on while the writer slept, so a save in flight
    /// gets a fresh window to show it is still writing (`record_power_win`).
    pub fn resumed(&mut self, now: Instant) {
        if self.pending_saves > 0 {
            self.save_progress = Some(now);
        }
        if self.write.last.is_some() {
            self.write.moved = Some(now);
        }
    }
}
