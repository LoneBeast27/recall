//! Win+L during a long recording (audit U8). Decision (coordinator, 2026-11-07; Rohan may
//! override): a notice only, no auto-stop. A stopped recording cannot be got back, extra
//! footage can be trimmed, and the disk-full guards cover the space. The mic and the replay
//! ring are not touched.
//! - The engine is told when the session locks and unlocks (`session_locked`); the final
//!   `recording_stopped` carries `locked_seconds` (the app and the island add "12:00 locked").
//! - On unlock, if a long recording ran during the lock, one notice: "Recording kept going
//!   while locked" / "12:00 locked" only when one recording ran from the lock to the unlock.
//!   One that ended during the lock (disk full, the game closed) says "Recording stopped
//!   while locked"; one that stopped and started again, or started during the lock, says
//!   "Recording ran for part of the lock". A lock of a minute or less is not news
//!   ([`NOTICE_OVER`], the same bar as the stop's note).
//!
//! This module only decides; `record_session_win` receives the WTS notices.

use std::time::{Duration, Instant};

/// A shorter lock gets no notice.
pub const NOTICE_OVER: Duration = Duration::from_secs(60);
/// The unlock notice's title: the recording is still running, or it stopped during the lock.
pub const KEPT_TITLE: &str = "Recording kept going while locked";
pub const STOPPED_TITLE: &str = "Recording stopped while locked";
pub const PARTLY_TITLE: &str = "Recording ran for part of the lock";

/// How a long recording fared over a lock.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LockOutcome {
    /// One recording ran from the lock to the unlock.
    Kept,
    /// It ran and is no longer running at the unlock.
    Stopped,
    /// Running at the unlock, but not the whole lock (it stopped and started again, or started).
    Partly,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LockAction {
    /// Send the engine `session_locked` with this value.
    TellEngine(bool),
    /// The unlock notice: the session was locked `span` during a recording.
    Notice { span: Duration, outcome: LockOutcome },
}

/// The session's lock state, as the WTS notices say.
#[derive(Debug, Default)]
pub struct LockWatch {
    locked_at: Option<Instant>,
    /// A long recording ran at some point of the current lock.
    recorded: bool,
    /// The recording that ran at the lock went on, unbroken, as far as the pump has seen.
    unbroken: bool,
}

impl LockWatch {
    pub fn is_locked(&self) -> bool {
        self.locked_at.is_some()
    }

    /// `WTS_SESSION_LOCK` at `now`. A repeat changes nothing.
    pub fn on_lock(&mut self, now: Instant, recording: bool) -> Vec<LockAction> {
        if self.locked_at.is_some() {
            return Vec::new();
        }
        self.locked_at = Some(now);
        self.recorded = recording;
        self.unbroken = recording;
        vec![LockAction::TellEngine(true)]
    }

    /// Every pump pass: a recording seen while locked counts for the unlock notice.
    pub fn recording_seen(&mut self, recording: bool) {
        if self.locked_at.is_none() {
            return;
        }
        self.recorded |= recording;
        self.unbroken &= recording;
    }

    /// `WTS_SESSION_UNLOCK` at `now`. With no lock seen, nothing.
    pub fn on_unlock(&mut self, now: Instant, recording: bool) -> Vec<LockAction> {
        let Some(at) = self.locked_at.take() else { return Vec::new() };
        let ran = std::mem::take(&mut self.recorded) || recording;
        let unbroken = std::mem::take(&mut self.unbroken) && recording;
        let outcome = match (unbroken, recording) {
            (true, _) => LockOutcome::Kept,
            (false, false) => LockOutcome::Stopped,
            (false, true) => LockOutcome::Partly,
        };
        let span = now.saturating_duration_since(at);
        let mut actions = vec![LockAction::TellEngine(false)];
        if ran && span > NOTICE_OVER {
            actions.push(LockAction::Notice { span, outcome });
        }
        actions
    }
}

/// "12:00 locked" (`m:ss`, `h:mm:ss` from an hour).
pub fn notice_detail(span: Duration) -> String {
    let total = span.as_secs();
    let (h, m, s) = (total / 3600, (total % 3600) / 60, total % 60);
    let length = if h > 0 { format!("{h}:{m:02}:{s:02}") } else { format!("{m}:{s:02}") };
    format!("{length} locked")
}

/// The unlock notice's title.
pub fn notice_title(outcome: LockOutcome) -> &'static str {
    match outcome {
        LockOutcome::Kept => KEPT_TITLE,
        LockOutcome::Stopped => STOPPED_TITLE,
        LockOutcome::Partly => PARTLY_TITLE,
    }
}

/// The app's event for the unlock notice (`session` / `recording_locked`,
/// `recording_stopped_locked` or `recording_partly_locked`).
pub fn app_event_json(span: Duration, outcome: LockOutcome) -> String {
    let kind = match outcome {
        LockOutcome::Kept => "recording_locked",
        LockOutcome::Stopped => "recording_stopped_locked",
        LockOutcome::Partly => "recording_partly_locked",
    };
    serde_json::json!({"type": kind, "locked_seconds": span.as_secs_f64()}).to_string()
}

/// The engine line for a lock change.
pub fn engine_line(locked: bool) -> String {
    serde_json::json!({"cmd": "session_locked", "locked": locked}).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    const MIN: Duration = Duration::from_secs(60);

    #[test]
    fn a_twelve_minute_lock_during_a_recording_gets_one_unlock_notice() {
        let t0 = Instant::now();
        let mut watch = LockWatch::default();
        assert_eq!(watch.on_lock(t0, true), vec![LockAction::TellEngine(true)]);
        watch.recording_seen(true);
        assert_eq!(watch.on_unlock(t0 + 12 * MIN, true), vec![LockAction::TellEngine(false), LockAction::Notice { span: 12 * MIN, outcome: LockOutcome::Kept }]);
        assert_eq!(watch.on_unlock(t0 + 13 * MIN, true), vec![], "one notice per lock");
        assert_eq!(notice_detail(12 * MIN), "12:00 locked");
    }

    #[test]
    fn a_lock_with_no_recording_gets_no_notice() {
        let t0 = Instant::now();
        let mut watch = LockWatch::default();
        watch.on_lock(t0, false);
        watch.recording_seen(false);
        assert_eq!(watch.on_unlock(t0 + 12 * MIN, false), vec![LockAction::TellEngine(false)]);
    }

    #[test]
    fn an_unlock_without_a_lock_does_nothing() {
        let mut watch = LockWatch::default();
        assert_eq!(watch.on_unlock(Instant::now(), true), vec![]);
        assert!(!watch.is_locked());
    }

    /// Gate 20b P2-d: a recording that stopped during the lock is not said to have kept going.
    #[test]
    fn a_recording_that_ran_during_the_lock_counts_and_a_short_lock_is_quiet() {
        let t0 = Instant::now();
        let mut watch = LockWatch::default();
        watch.on_lock(t0, false);
        assert!(watch.is_locked());
        watch.on_lock(t0 + MIN, true); // a repeated lock keeps the first time
        watch.recording_seen(true); // it ran for a while, then stopped (disk full)
        assert_eq!(watch.on_unlock(t0 + 5 * MIN, false), vec![LockAction::TellEngine(false), LockAction::Notice { span: 5 * MIN, outcome: LockOutcome::Stopped }]);
        assert_eq!(notice_title(LockOutcome::Stopped), "Recording stopped while locked");
        assert_eq!(notice_title(LockOutcome::Kept), "Recording kept going while locked");
        watch.on_lock(t0 + 6 * MIN, true);
        assert_eq!(watch.on_unlock(t0 + 6 * MIN + Duration::from_secs(40), true), vec![LockAction::TellEngine(false)], "40 s is not news");
    }

    /// Gate 20b v3 P2-4: a stop and a start again within one lock (a cancelled shutdown on
    /// the lock screen), or a start during it, never "kept going" for the whole lock.
    #[test]
    fn a_recording_broken_or_started_during_the_lock_ran_for_part_of_it() {
        let t0 = Instant::now();
        let mut watch = LockWatch::default();
        watch.on_lock(t0, true);
        watch.recording_seen(true);
        watch.recording_seen(false); // stopped
        watch.recording_seen(true); // started again
        assert_eq!(watch.on_unlock(t0 + 12 * MIN, true), vec![LockAction::TellEngine(false), LockAction::Notice { span: 12 * MIN, outcome: LockOutcome::Partly }]);
        watch.on_lock(t0 + 13 * MIN, false);
        watch.recording_seen(true);
        assert_eq!(watch.on_unlock(t0 + 20 * MIN, true)[1], LockAction::Notice { span: 7 * MIN, outcome: LockOutcome::Partly });
        assert_eq!(notice_title(LockOutcome::Partly), "Recording ran for part of the lock");
    }

    #[test]
    fn the_wire_shapes() {
        assert_eq!(engine_line(true), r#"{"cmd":"session_locked","locked":true}"#);
        let app: serde_json::Value = serde_json::from_str(&app_event_json(12 * MIN, LockOutcome::Kept)).unwrap();
        assert_eq!(app["type"], "recording_locked");
        assert_eq!(app["locked_seconds"].as_f64(), Some(720.0));
        let stopped: serde_json::Value = serde_json::from_str(&app_event_json(12 * MIN, LockOutcome::Stopped)).unwrap();
        assert_eq!(stopped["type"], "recording_stopped_locked");
        let partly: serde_json::Value = serde_json::from_str(&app_event_json(12 * MIN, LockOutcome::Partly)).unwrap();
        assert_eq!(partly["type"], "recording_partly_locked");
        assert_eq!(notice_detail(Duration::from_secs(3725)), "1:02:05 locked");
    }
}
