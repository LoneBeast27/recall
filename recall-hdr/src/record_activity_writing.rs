//! The engine's `status.save_writing` (`{"bytes":N,"queued":M,"step":S}`: the save being
//! written and the saves behind it) as the controller follows it. A mark that changes
//! (`step`, the saver's work counter, from the audio encode on; `bytes` from an engine
//! without it) shows the save is still being written, so the queue's window moves on and
//! every restart kind waits; a mark stuck for [`SAVE_STALE`] is a wedged writer and
//! expires as before. An engine without the field (or `null`) reports nothing here, so
//! only the plain window applies (today's behaviour).
//!
//! A save that keeps growing holds restarts at most [`hold_cap`] (a dying drive must not
//! keep the next game unrecorded for an hour): then restarts go ahead as before the hold
//! existed, and a restart held [`HOLD_NOTICE_AFTER`] says so on the island once.
//!
//! Also here: answers that arrive for a queue that already expired. They drain that
//! queue's count, never the queue sent since (an expired save's late `saved` would
//! otherwise let a restart kill the newer saves).

use std::time::Duration;

use crate::awake_clock::Awake;

use serde_json::Value;

use super::{EngineActivity, SAVE_STALE, SETTLE_NO_HEARTBEAT};
use crate::log;

/// The progress mark of `status.save_writing`: its `step` when the engine sends one,
/// else its `bytes`. `None` when the line reports no save in the saver (an engine
/// without the field never reports one; `null` is none too).
pub fn writing_mark(ev: &Value) -> Option<u64> {
    let w = ev.get("save_writing").filter(|w| w.is_object())?;
    let mark = w.get("step").and_then(Value::as_u64).or_else(|| w.get("bytes").and_then(Value::as_u64));
    Some(mark.unwrap_or(0))
}

/// The shortest cap on how long a growing save holds restarts.
pub const SAVE_HOLD_MIN: Duration = Duration::from_secs(600);
/// The drive speed the cap assumes for the clip's estimated size (bytes/s).
pub const SAVE_HOLD_RATE: u64 = 5_000_000;
/// Progress this recent means "still growing" (a hang restart, a quit).
pub const GROWING_WITHIN: Duration = Duration::from_secs(10);
/// A restart held this long by a save says so on the island (once per save).
pub const HOLD_NOTICE_AFTER: Duration = Duration::from_secs(30);

/// The longest one growing save holds restarts: max(10 min, its size at 5 MB/s).
pub fn hold_cap(estimate: u64) -> Duration {
    SAVE_HOLD_MIN.max(Duration::from_secs(estimate / SAVE_HOLD_RATE))
}

/// The last `save_writing` mark seen, when it last changed, and when the save in
/// front started (for its cap).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct WriteWatch {
    last: Option<u64>,
    /// The packet bytes of the last report (`save_writing.bytes`; none: no report).
    bytes: Option<u64>,
    moved: Option<Awake>,
    since: Option<Awake>,
    /// The save in front ran past its cap (its queue was let go once).
    capped: bool,
    /// A restart held by the save in front: since when, and whether the island was told.
    held_since: Option<Awake>,
    told: bool,
}

impl WriteWatch {
    /// One status line at `now`. True when the save moved on: a save appeared, or
    /// its mark changed (a new save in front starts small again). A press that
    /// only grows the queue is not progress.
    pub fn status(&mut self, ev: &Value, now: Awake) -> bool {
        let seen = writing_mark(ev);
        if seen.is_none() {
            self.next_save(None);
        } else if self.last.is_none() {
            self.next_save(Some(now));
        }
        let moved = seen.is_some() && seen != self.last;
        if moved {
            self.moved = Some(now);
        }
        self.last = seen;
        self.bytes = ev.get("save_writing").and_then(|w| w.get("bytes")).and_then(Value::as_u64).filter(|_| seen.is_some());
        moved
    }

    /// The save in front changed at `now` (or the saver went empty: `None`).
    fn next_save(&mut self, now: Option<Awake>) {
        (self.since, self.capped, self.held_since, self.told) = (now, false, None, false);
    }

    /// The save in front has been written longer than `cap`.
    fn past_cap(&self, now: Awake, cap: Duration) -> bool {
        self.last.is_some() && self.since.is_some_and(|s| now.saturating_duration_since(s) >= cap)
    }

    /// The engine reports a save whose mark moved within `within`, under its cap.
    fn moved_within(&self, now: Awake, within: Duration, cap: Duration) -> bool {
        self.last.is_some() && self.moved.is_some_and(|t| now.saturating_duration_since(t) < within) && !self.past_cap(now, cap)
    }
}

impl EngineActivity {
    fn hold_cap(&self) -> Duration {
        hold_cap(self.save_estimate)
    }

    /// A `status` line: a save whose mark moves keeps its queue's window open (not
    /// past its cap).
    pub(super) fn on_status(&mut self, ev: &Value, now: Awake) {
        let cap = self.hold_cap();
        if self.write.status(ev, now) && self.pending_saves > 0 && !self.write.past_cap(now, cap) {
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

    /// A save was answered at `now`: the next one in the saver is in front now.
    pub(super) fn front_answered(&mut self, now: Awake) {
        if self.write.last.is_some() {
            self.write.next_save(Some(now));
        }
    }

    /// A save (or screenshot) the engine has not answered, or one it reports writing: what a
    /// privacy stop must not cut (gate p5-1). No timer and no cap: a save past its 10 min
    /// cap is still written by the engine, and only `saved` / `save_failed`, a `status`
    /// without `save_writing` or the engine's exit end it.
    pub fn save_in_flight(&self) -> bool {
        self.pending_saves + self.expired_saves > 0 || self.write.last.is_some() || self.pending_screenshots > 0
    }

    /// The engine was released at `now` (`Transitions::drop_ring`, design P2); `heartbeats`: its
    /// hello says `drain_heartbeat` (`Supervisor::engine_takes_drain_heartbeat`).
    pub fn released(&mut self, now: Awake, heartbeats: bool) {
        (self.released_at, self.drain_heartbeats) = (Some(now), heartbeats);
    }

    /// When this engine was released, and whether it sends drain heartbeats.
    pub fn release(&self) -> Option<(Awake, bool)> {
        self.released_at.map(|at| (at, self.drain_heartbeats))
    }

    /// A released engine may be cut to the quit grace (`Supervisor::release_settled`): every
    /// save it was sent is answered (`saved` / `save_failed`; an engine exit answers them all).
    /// Design P7: an engine that sends drain heartbeats is the only judge of a stalled save (it
    /// gives it up itself, with an answer), so no timer settles it here. P8: an older engine
    /// with `stop_capture` but no heartbeat settles once no save showed progress for
    /// [`SETTLE_NO_HEARTBEAT`] (awake), counted from the last progress or the release,
    /// whichever is later (the engine's own stall clock starts at the release, p5-1).
    pub fn release_may_settle(&mut self, now: Awake) -> bool {
        if self.pending_saves + self.expired_saves == 0 {
            return true;
        }
        if self.drain_heartbeats {
            return false;
        }
        let last = self.save_progress.max(self.write.moved).max(self.released_at);
        last.is_none_or(|t| now.saturating_duration_since(t) >= SETTLE_NO_HEARTBEAT)
    }

    /// Saves the engine is still writing: counted, or reported by the engine with
    /// its mark still moving. A restart now aborts them. A save past its cap lets
    /// its queue go (once): restarts go ahead as they did before this hold.
    pub fn saves_writing(&mut self, now: Awake) -> bool {
        self.expire_saves(now);
        let cap = self.hold_cap();
        if self.write.past_cap(now, cap) && !self.write.capped {
            self.write.capped = true;
            log!("record: a save has been writing for {} s (its cap); restarts no longer wait for it", cap.as_secs());
            self.expired_saves += std::mem::take(&mut self.pending_saves);
        }
        self.pending_saves > 0 || self.write.moved_within(now, SAVE_STALE, cap)
    }

    /// The PC woke up: the clock ran on while the writer slept, so a save in flight
    /// gets a fresh window (and a fresh cap) to show it is still writing
    /// (`record_power_win`).
    pub fn resumed(&mut self, now: Awake) {
        if self.pending_saves > 0 {
            self.save_progress = Some(now);
        }
        if self.write.last.is_some() {
            self.write.moved = Some(now);
            self.write.since = Some(now);
        }
    }

    /// The clip size a save is expected to have (`record_free_space::buffer_bytes`).
    pub fn set_save_estimate(&mut self, bytes: u64) {
        self.save_estimate = bytes;
    }

    /// The engine reports the save in front still making progress (within
    /// [`GROWING_WITHIN`], under its cap). An engine without the report never is.
    pub fn save_growing(&mut self, now: Awake) -> bool {
        let cap = self.hold_cap();
        self.write.moved_within(now, GROWING_WITHIN, cap)
    }

    /// The last progress mark the engine reported for the save in front (none: no report).
    pub fn writing_mark(&self) -> Option<u64> {
        self.write.last
    }

    /// The bytes the engine last reported written for the save in front (none: no report).
    pub fn writing_bytes(&self) -> Option<u64> {
        self.write.bytes
    }

    /// Saves in flight a quit now would lose.
    pub fn saves_in_flight(&self) -> u32 {
        self.pending_saves.max(self.write.last.is_some() as u32)
    }

    /// A restart is `held` now: true once per save when a save has held it for
    /// [`HOLD_NOTICE_AFTER`] (the island says "Waiting for the clip to finish saving").
    pub fn hold_notice(&mut self, now: Awake, held: bool) -> bool {
        let writing = self.saves_writing(now);
        if !(held && writing) {
            self.write.held_since = None;
            self.write.told &= writing; // nothing in flight: the next save may say so again
            return false;
        }
        let since = *self.write.held_since.get_or_insert(now);
        if self.write.told || now.saturating_duration_since(since) < HOLD_NOTICE_AFTER {
            return false;
        }
        self.write.told = true;
        true
    }
}
