//! What the engine is busy with, from the controller's side: replay saves in
//! flight and the manual long recording. Settings changes and retargets that
//! need an engine restart wait until neither is running (`pending_restart` in
//! the session status), and a pause first stops the long recording
//! ([`PendingPause`]); engine events arrive through `Supervisor::attach_events`.

use std::time::{Duration, Instant};

use serde_json::{json, Value};

/// A save with no `saved` / `save_failed` after this is assumed lost.
pub const SAVE_STALE: Duration = Duration::from_secs(120);
/// A `record_start` with no `recording_started` / `_failed` after this is dropped
/// (the engine's own keyframe wait is 30 s).
pub const START_STALE: Duration = Duration::from_secs(35);
/// How long a pause waits for the long recording to report stopping.
pub const PAUSE_STOP_WAIT: Duration = Duration::from_secs(10);

#[derive(Debug, Clone, PartialEq)]
pub enum LongRec {
    Idle,
    Starting { since: Instant },
    /// `started_unix`: Unix seconds when the engine reported it running.
    Active { path: String, started_unix: f64 },
}

#[derive(Debug, Clone, PartialEq)]
pub struct EngineActivity {
    pending_saves: u32,
    last_save_sent: Option<Instant>,
    pub long: LongRec,
}

impl Default for EngineActivity {
    fn default() -> Self {
        EngineActivity { pending_saves: 0, last_save_sent: None, long: LongRec::Idle }
    }
}

/// An engine event the controller acts on beyond bookkeeping.
#[derive(Debug, Clone, PartialEq)]
pub enum Trigger {
    /// A clip landed: run the storage cap.
    Saved,
    /// Long recording started / stopped / failed: publish status.
    LongRecChanged,
}

impl EngineActivity {
    pub fn save_sent(&mut self, now: Instant) {
        self.pending_saves += 1;
        self.last_save_sent = Some(now);
    }

    /// A `record_start` / `record_toggle` went to an idle engine.
    pub fn record_requested(&mut self, now: Instant) {
        if self.long == LongRec::Idle {
            self.long = LongRec::Starting { since: now };
        }
    }

    pub fn is_recording(&self) -> bool {
        matches!(self.long, LongRec::Active { .. })
    }

    /// A new engine instance: nothing of the old one is in flight any more.
    pub fn engine_restarted(&mut self) {
        *self = EngineActivity::default();
    }

    /// Track one engine event; `unix_now` stamps a recording start.
    pub fn on_event(&mut self, ev: &Value, unix_now: f64) -> Option<Trigger> {
        let text = |k: &str| ev.get(k).and_then(Value::as_str).unwrap_or("").to_string();
        match ev.get("type").and_then(Value::as_str)? {
            "saved" => {
                self.pending_saves = self.pending_saves.saturating_sub(1);
                Some(Trigger::Saved)
            }
            "save_failed" => {
                self.pending_saves = self.pending_saves.saturating_sub(1);
                None
            }
            "recording_started" => {
                self.long = LongRec::Active { path: text("path"), started_unix: unix_now };
                Some(Trigger::LongRecChanged)
            }
            "recording_stopped" | "recording_failed" => {
                self.long = LongRec::Idle;
                Some(Trigger::LongRecChanged)
            }
            _ => None,
        }
    }

    /// Saves in flight or a long recording starting / running: an engine
    /// restart now would lose work.
    pub fn busy(&mut self, now: Instant) -> bool {
        if self.last_save_sent.is_some_and(|t| now.duration_since(t) >= SAVE_STALE) {
            self.pending_saves = 0;
        }
        if let LongRec::Starting { since } = self.long {
            if now.duration_since(since) >= START_STALE {
                self.long = LongRec::Idle;
            }
        }
        self.pending_saves > 0 || self.long != LongRec::Idle
    }

    /// Session-status `long_recording`.
    pub fn long_recording_json(&self) -> Value {
        match &self.long {
            LongRec::Active { path, started_unix } => json!({"active": true, "path": path, "started_secs": started_unix}),
            LongRec::Starting { .. } => json!({"active": false, "starting": true, "path": null, "started_secs": null}),
            LongRec::Idle => json!({"active": false, "path": null, "started_secs": null}),
        }
    }
}

/// A pause held back while the long recording stops (its `record_stop` is
/// sent): it applies once the engine reports `recording_stopped` /
/// `recording_failed`, or after [`PAUSE_STOP_WAIT`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PendingPause {
    deadline: Instant,
}

impl PendingPause {
    /// `Some` when a pause asked at `now` must first stop the long recording
    /// (starting or running).
    pub fn needed(activity: &EngineActivity, now: Instant) -> Option<PendingPause> {
        (activity.long != LongRec::Idle).then(|| PendingPause { deadline: now + PAUSE_STOP_WAIT })
    }

    /// The recording reported its end, or the wait ran out.
    pub fn ready(&self, activity: &EngineActivity, now: Instant) -> bool {
        activity.long == LongRec::Idle || now >= self.deadline
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_pause_waits_for_the_long_recording_to_stop_but_not_forever() {
        let t0 = Instant::now();
        let mut a = EngineActivity::default();
        assert_eq!(PendingPause::needed(&a, t0), None, "nothing recording: pause at once");
        a.record_requested(t0);
        assert!(PendingPause::needed(&a, t0).is_some(), "a starting recording is stopped first too");
        a.on_event(&json!({"type":"recording_started","path":"p"}), 1.0);
        let wait = PendingPause::needed(&a, t0).expect("recording: stop first");
        assert!(!wait.ready(&a, t0 + Duration::from_secs(1)));
        a.on_event(&json!({"type":"recording_stopped","path":"p","seconds":1.0,"bytes":1}), 0.0);
        assert!(wait.ready(&a, t0 + Duration::from_secs(1)), "stopped: pause now");
        a.on_event(&json!({"type":"recording_started","path":"p"}), 1.0);
        assert!(!wait.ready(&a, t0 + PAUSE_STOP_WAIT - Duration::from_millis(1)));
        assert!(wait.ready(&a, t0 + PAUSE_STOP_WAIT), "an engine that never answers does not block the pause");
    }

    #[test]
    fn saves_in_flight_block_restarts_until_reported_or_stale() {
        let t0 = Instant::now();
        let mut a = EngineActivity::default();
        assert!(!a.busy(t0));
        a.save_sent(t0);
        a.save_sent(t0);
        assert!(a.busy(t0));
        assert_eq!(a.on_event(&json!({"type":"saved","path":"x.mp4"}), 0.0), Some(Trigger::Saved));
        assert!(a.busy(t0), "one more in flight");
        assert_eq!(a.on_event(&json!({"type":"save_failed","reason":"r"}), 0.0), None);
        assert!(!a.busy(t0));
        a.save_sent(t0);
        assert!(!a.busy(t0 + SAVE_STALE), "a lost save does not block forever");
    }

    #[test]
    fn long_recording_lifecycle_and_status_shape() {
        let t0 = Instant::now();
        let mut a = EngineActivity::default();
        assert_eq!(a.long_recording_json(), json!({"active":false,"path":null,"started_secs":null}));
        a.record_requested(t0);
        assert!(a.busy(t0) && !a.is_recording());
        a.on_event(&json!({"type":"recording_started","path":"V:\\c\\G__rec.mp4"}), 1_790_000_000.5);
        assert!(a.is_recording());
        assert_eq!(
            a.long_recording_json(),
            json!({"active":true,"path":"V:\\c\\G__rec.mp4","started_secs":1_790_000_000.5})
        );
        assert_eq!(a.on_event(&json!({"type":"recording_stopped","path":"p","seconds":3.0,"bytes":9}), 0.0), Some(Trigger::LongRecChanged));
        assert!(!a.busy(t0));
        a.record_requested(t0);
        assert!(!a.busy(t0 + START_STALE), "a start that never answered expires");
        a.record_requested(t0);
        a.on_event(&json!({"type":"recording_failed","reason":"x","path":null}), 0.0);
        assert_eq!(a.long, LongRec::Idle);
    }

    #[test]
    fn a_fresh_engine_clears_everything() {
        let t0 = Instant::now();
        let mut a = EngineActivity::default();
        a.save_sent(t0);
        a.on_event(&json!({"type":"recording_started","path":"p"}), 1.0);
        a.engine_restarted();
        assert_eq!(a, EngineActivity::default());
    }
}
