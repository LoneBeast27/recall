//! Sign out, restart and shut down (audit U6). Without this the session end killed the
//! engine instead of quitting it: a running long recording was left as `.part` (recovered,
//! a few seconds short, on the next start).
//! - `WM_QUERYENDSESSION` with a long recording running: Windows is told why we hold on
//!   ("Saving your recording"), the engine gets `record_stop`, and the pump waits up to
//!   [`STOP_WAIT`] for `recording_stopped` before answering. The answer is always yes: Recall
//!   never refuses a shutdown. With no recording nothing happens.
//! - `WM_ENDSESSION` (the session really ends): the controller's clean shutdown runs before
//!   the handler returns, the engine quit capped at [`SESSION_END_GRACE`] (Windows may end the
//!   process as soon as the handler returns).
//!
//! - `WM_ENDSESSION` with `fEnding` FALSE (another app cancelled the shutdown): a long
//!   recording the query stopped (or one still starting then) starts again in a new file,
//!   with the notice "Shutdown cancelled" / "Recording continues in a new file".
//! - Lock notices need the WTS service, which may not be up yet at boot: a failed
//!   registration is retried every [`REGISTER_RETRY_EVERY`], up to [`REGISTER_RETRIES`] times.
//!
//! This module only decides; `record_session_win` owns the hidden window that receives the
//! messages (and the session lock notices, `record_lock`) and applies the actions.

use std::time::{Duration, Instant};

/// `WM_QUERYENDSESSION`.
pub const WM_QUERYENDSESSION: u32 = 0x0011;
/// `WM_ENDSESSION`.
pub const WM_ENDSESSION: u32 = 0x0016;
/// `WM_WTSSESSION_CHANGE`.
pub const WM_WTSSESSION_CHANGE: u32 = 0x02B1;
/// `WTS_SESSION_LOCK` / `WTS_SESSION_UNLOCK` (the message's wParam).
pub const WTS_SESSION_LOCK: usize = 0x7;
pub const WTS_SESSION_UNLOCK: usize = 0x8;

/// What Windows is shown while the stop runs (Settings' "apps preventing shutdown" list).
pub const BLOCK_REASON: &str = "Saving your recording";
/// How long the query waits for `recording_stopped`.
pub const STOP_WAIT: Duration = Duration::from_secs(3);
/// The engine's quit grace at a session end (the normal quit allows 8 s).
pub const SESSION_END_GRACE: Duration = Duration::from_secs(3);
/// The notice when a cancelled shutdown restarts the long recording.
pub const CANCEL_TITLE: &str = "Shutdown cancelled";
pub const CANCEL_DETAIL: &str = "Recording continues in a new file";
/// A failed `WTSRegisterSessionNotification` is tried again this often, this many times.
pub const REGISTER_RETRY_EVERY: Duration = Duration::from_secs(10);
pub const REGISTER_RETRIES: u32 = 5;

/// The hidden window's messages that matter.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionMsg {
    QueryEnd,
    /// `ending`: the session really ends (false: the shutdown was cancelled).
    End { ending: bool },
    Lock,
    Unlock,
}

impl SessionMsg {
    pub fn classify(msg: u32, wparam: usize) -> Option<Self> {
        match (msg, wparam) {
            (WM_QUERYENDSESSION, _) => Some(Self::QueryEnd),
            (WM_ENDSESSION, w) => Some(Self::End { ending: w != 0 }),
            (WM_WTSSESSION_CHANGE, WTS_SESSION_LOCK) => Some(Self::Lock),
            (WM_WTSSESSION_CHANGE, WTS_SESSION_UNLOCK) => Some(Self::Unlock),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EndAction {
    /// `ShutdownBlockReasonCreate` with this text.
    Block(&'static str),
    /// Send the engine `record_stop`.
    StopRecording,
    /// Pump engine events until the recording reports stopping, or this long.
    WaitStopped(Duration),
    /// `ShutdownBlockReasonDestroy`.
    Unblock,
    /// The controller's clean shutdown, engine quit capped at this grace.
    Shutdown(Duration),
    /// The shutdown was cancelled: start the long recording again, with the notice.
    Restart,
}

/// What a session end has done so far.
#[derive(Debug, Default)]
pub struct SessionEnd {
    /// The query stopped a long recording (running or starting): a cancel restarts it.
    stopped_at_query: bool,
}

impl SessionEnd {
    /// `WM_QUERYENDSESSION`: with a long recording running or starting, save it before answering.
    pub fn on_query_end(&mut self, recording: bool) -> Vec<EndAction> {
        if !recording {
            return Vec::new();
        }
        self.stopped_at_query = true;
        vec![EndAction::Block(BLOCK_REASON), EndAction::StopRecording, EndAction::WaitStopped(STOP_WAIT), EndAction::Unblock]
    }

    /// A query that arrived outside the pump's drain (already answered): stop, no wait.
    pub fn on_late_query_end(&mut self, recording: bool) -> Vec<EndAction> {
        if !recording {
            return Vec::new();
        }
        self.stopped_at_query = true;
        vec![EndAction::StopRecording]
    }

    /// `WM_ENDSESSION`: shut down cleanly when the session really ends; when it was
    /// cancelled, restart a recording the query stopped.
    pub fn on_end(&mut self, ending: bool) -> Vec<EndAction> {
        let stopped = std::mem::take(&mut self.stopped_at_query);
        match (ending, stopped) {
            (true, _) => vec![EndAction::Shutdown(SESSION_END_GRACE)],
            (false, true) => vec![EndAction::Restart],
            (false, false) => Vec::new(),
        }
    }
}

/// The app's event for the cancelled-shutdown notice (`session` / `shutdown_cancelled`).
pub fn cancel_event_json() -> String {
    serde_json::json!({"type": "shutdown_cancelled", "title": CANCEL_TITLE, "detail": CANCEL_DETAIL}).to_string()
}

/// The answer to `WM_QUERYENDSESSION`: always TRUE (never hold up a shutdown).
pub const QUERY_ANSWER: isize = 1;

/// Retries of a `WTSRegisterSessionNotification` that failed at start-up.
#[derive(Debug, PartialEq, Eq)]
pub struct RegisterRetry {
    next: Option<Instant>,
    left: u32,
}

impl RegisterRetry {
    /// Nothing to retry.
    pub const NONE: RegisterRetry = RegisterRetry { next: None, left: 0 };

    /// The first registration failed at `now`.
    pub fn failed_at(now: Instant) -> Self {
        RegisterRetry { next: Some(now + REGISTER_RETRY_EVERY), left: REGISTER_RETRIES }
    }

    /// A retry is due now.
    pub fn due(&self, now: Instant) -> bool {
        self.next.is_some_and(|at| now >= at)
    }

    /// The outcome of a retry made at `now`: a success or the last failure ends the retries.
    pub fn tried(&mut self, ok: bool, now: Instant) {
        self.left = self.left.saturating_sub(1);
        self.next = (!ok && self.left > 0).then(|| now + REGISTER_RETRY_EVERY);
        if ok {
            self.left = 0;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const T10: Duration = Duration::from_secs(10);

    #[test]
    fn a_shutdown_with_no_recording_does_nothing_until_the_session_ends() {
        let mut end = SessionEnd::default();
        assert_eq!(end.on_query_end(false), vec![]);
        assert_eq!(end.on_end(false), vec![], "a cancelled shutdown with nothing stopped");
        assert_eq!(end.on_end(true), vec![EndAction::Shutdown(SESSION_END_GRACE)]);
    }

    /// Gate 20b P1: a shutdown another app cancels restarts the recording the query stopped.
    #[test]
    fn a_cancelled_shutdown_restarts_the_recording_the_query_stopped_once() {
        let mut end = SessionEnd::default();
        assert!(end.on_query_end(true).contains(&EndAction::StopRecording));
        assert_eq!(end.on_query_end(false), vec![], "a second query: already stopped");
        assert_eq!(end.on_end(false), vec![EndAction::Restart]);
        assert_eq!(end.on_end(false), vec![], "one restart per stop");
        end.on_late_query_end(true);
        assert_eq!(end.on_end(true), vec![EndAction::Shutdown(SESSION_END_GRACE)], "a real end never restarts");
        assert_eq!(end.on_end(false), vec![]);
    }

    #[test]
    fn the_cancel_event_for_the_app() {
        let v: serde_json::Value = serde_json::from_str(&cancel_event_json()).unwrap();
        assert_eq!(v["type"], "shutdown_cancelled");
        assert_eq!(v["detail"], "Recording continues in a new file");
    }

    /// Gate 20b P2-c: a lock-notice registration that failed at boot is retried every 10 s, 5 times.
    #[test]
    fn a_failed_registration_is_retried_every_10_s_five_times() {
        let t0 = Instant::now();
        let mut retry = RegisterRetry::failed_at(t0);
        assert!(!retry.due(t0 + Duration::from_secs(9)));
        let mut tries = 0;
        for step in 1..=8u32 {
            let now = t0 + step * T10;
            if retry.due(now) {
                tries += 1;
                retry.tried(false, now);
            }
        }
        assert_eq!(tries, 5);
        assert_eq!(retry, RegisterRetry::NONE);
        let mut retry = RegisterRetry::failed_at(t0);
        retry.tried(true, t0 + T10);
        assert!(!retry.due(t0 + 5 * T10), "a success ends the retries");
        assert!(!RegisterRetry::NONE.due(t0 + T10));
    }

    #[test]
    fn only_the_session_messages_are_taken() {
        assert_eq!(SessionMsg::classify(0x0011, 0), Some(SessionMsg::QueryEnd));
        assert_eq!(SessionMsg::classify(0x0016, 1), Some(SessionMsg::End { ending: true }));
        assert_eq!(SessionMsg::classify(0x0016, 0), Some(SessionMsg::End { ending: false }));
        assert_eq!(SessionMsg::classify(0x02B1, 7), Some(SessionMsg::Lock));
        assert_eq!(SessionMsg::classify(0x02B1, 8), Some(SessionMsg::Unlock));
        assert_eq!(SessionMsg::classify(0x02B1, 5), None, "WTS_SESSION_LOGON");
        assert_eq!(SessionMsg::classify(0x0010, 0), None, "WM_CLOSE");
    }
}
