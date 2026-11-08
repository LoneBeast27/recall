//! Sign out, restart and shut down (audit U6). Without this the session end killed the
//! engine instead of quitting it: a running long recording was left as `.part` (recovered,
//! a few seconds short, on the next start).
//! - `WM_QUERYENDSESSION` with a long recording running: Windows is told why we hold on
//!   ("Saving your recording"), the engine gets `record_stop`, and the pump waits up to
//!   [`STOP_WAIT`] for `recording_stopped` before answering. The session window holds a query while a replay save is writing.
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
/// While `Global\TermSrvReadyEvent` is unset the retries wait (uncounted), at most this long
/// after the first failure.
pub const REGISTER_SERVICE_WAIT: Duration = Duration::from_secs(300);

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

/// The long recording as a session-end query finds it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LongState {
    /// None running or starting.
    Idle,
    /// Running or starting, no stop sent.
    Running,
    /// A `record_stop` is on its way (the user's, a pause, a retarget, sleep, or ours).
    Stopping,
}

impl LongState {
    /// From the controller's activity: a recording running or starting, a stop on its way.
    pub fn of(recording_or_starting: bool, stop_in_flight: bool) -> Self {
        match (recording_or_starting, stop_in_flight) {
            (false, _) => LongState::Idle,
            (true, false) => LongState::Running,
            (true, true) => LongState::Stopping,
        }
    }
}

/// A cancel's restart went out: the engine was idle before it and is starting now (a refused
/// start, one the engine is not reading, or one sent to a recording still running is not).
pub fn start_went_out(was_idle: bool, starting_now: bool) -> bool {
    was_idle && starting_now
}

/// What a session end has done so far.
#[derive(Debug, Default)]
pub struct SessionEnd {
    /// The query stopped a long recording (running or starting): a cancel restarts it.
    stopped_at_query: bool,
}

impl SessionEnd {
    /// `WM_QUERYENDSESSION`: a long recording running or starting is stopped and saved before
    /// the answer, and only that one is restarted on a cancel. One already stopping (a stop
    /// the user or Recall sent) is waited for but never restarted; with none, nothing.
    pub fn on_query_end(&mut self, long: LongState) -> Vec<EndAction> {
        match long {
            LongState::Idle => {
                self.stopped_at_query = false; // a stale arm (the recording ended since) restarts nothing
                Vec::new()
            }
            LongState::Stopping => vec![EndAction::Block(BLOCK_REASON), EndAction::WaitStopped(STOP_WAIT), EndAction::Unblock],
            LongState::Running => {
                self.stopped_at_query = true;
                vec![EndAction::Block(BLOCK_REASON), EndAction::StopRecording, EndAction::WaitStopped(STOP_WAIT), EndAction::Unblock]
            }
        }
    }

    /// A query that arrived outside the pump's drain (already answered): stop, no wait.
    pub fn on_late_query_end(&mut self, long: LongState) -> Vec<EndAction> {
        match long {
            LongState::Idle => {
                self.stopped_at_query = false;
                Vec::new()
            }
            LongState::Stopping => Vec::new(),
            LongState::Running => {
                self.stopped_at_query = true;
                vec![EndAction::StopRecording]
            }
        }
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

/// The answer to `WM_QUERYENDSESSION` when there is no replay save to hold it.
pub const QUERY_ANSWER: isize = 1;

/// Retries of a `WTSRegisterSessionNotification` that failed at start-up.
#[derive(Debug, PartialEq, Eq)]
pub struct RegisterRetry {
    next: Option<Instant>,
    left: u32,
    /// The first failure (the service wait counts from it).
    since: Option<Instant>,
}

impl RegisterRetry {
    /// Nothing to retry.
    pub const NONE: RegisterRetry = RegisterRetry { next: None, left: 0, since: None };

    /// The first registration failed at `now`.
    pub fn failed_at(now: Instant) -> Self {
        RegisterRetry { next: Some(now + REGISTER_RETRY_EVERY), left: REGISTER_RETRIES, since: Some(now) }
    }

    /// A retry is due now.
    pub fn due(&self, now: Instant) -> bool {
        self.next.is_some_and(|at| now >= at)
    }

    /// The WTS service is not up yet at a due retry: wait another round without spending a
    /// try, while within [`REGISTER_SERVICE_WAIT`] of the first failure. False = try anyway.
    pub fn wait_for_service(&mut self, now: Instant) -> bool {
        let waiting = self.since.is_some_and(|since| now.saturating_duration_since(since) < REGISTER_SERVICE_WAIT);
        if waiting {
            self.next = Some(now + REGISTER_RETRY_EVERY);
        }
        waiting
    }

    /// The outcome of a retry made at `now`: a success or the last failure ends the retries.
    pub fn tried(&mut self, ok: bool, now: Instant) {
        self.left = self.left.saturating_sub(1);
        self.next = (!ok && self.left > 0).then(|| now + REGISTER_RETRY_EVERY);
        if self.next.is_none() {
            *self = RegisterRetry::NONE;
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
        assert_eq!(end.on_query_end(LongState::Idle), vec![]);
        assert_eq!(end.on_end(false), vec![], "a cancelled shutdown with nothing stopped");
        assert_eq!(end.on_end(true), vec![EndAction::Shutdown(SESSION_END_GRACE)]);
    }

    /// Gate 20b P1: a shutdown another app cancels restarts the recording the query stopped.
    #[test]
    fn a_cancelled_shutdown_restarts_the_recording_the_query_stopped_once() {
        let mut end = SessionEnd::default();
        assert!(end.on_query_end(LongState::Running).contains(&EndAction::StopRecording));
        assert!(!end.on_query_end(LongState::Stopping).contains(&EndAction::StopRecording), "a second query while our stop runs");
        assert_eq!(end.on_end(false), vec![EndAction::Restart]);
        assert_eq!(end.on_end(false), vec![], "one restart per stop");
        end.on_late_query_end(LongState::Running);
        assert_eq!(end.on_end(true), vec![EndAction::Shutdown(SESSION_END_GRACE)], "a real end never restarts");
        assert_eq!(end.on_end(false), vec![]);
    }

    /// Gate 20b v3 P1: a stop the user (or a pause / retarget / sleep) sent before the query
    /// is waited for, never undone by a cancel.
    #[test]
    fn a_stop_already_in_flight_at_the_query_is_saved_but_never_restarted() {
        let mut end = SessionEnd::default();
        assert_eq!(end.on_query_end(LongState::Stopping), vec![EndAction::Block(BLOCK_REASON), EndAction::WaitStopped(STOP_WAIT), EndAction::Unblock]);
        assert_eq!(end.on_end(false), vec![]);
        assert_eq!(end.on_late_query_end(LongState::Stopping), vec![]);
        assert_eq!(end.on_end(false), vec![]);
    }

    /// Gate 20b v3 P2-1: a query that finds nothing recording clears an earlier arm.
    #[test]
    fn a_query_that_finds_nothing_recording_clears_a_stale_arm() {
        let mut end = SessionEnd::default();
        end.on_query_end(LongState::Running);
        end.on_query_end(LongState::Idle);
        assert_eq!(end.on_end(false), vec![]);
        end.on_late_query_end(LongState::Running);
        end.on_late_query_end(LongState::Idle);
        assert_eq!(end.on_end(false), vec![]);
    }

    /// Gate 20b v3 P2-2/P2-5 (the controller host's pieces): a stop in flight is Stopping, only
    /// an idle-to-starting change counts as the restart going out.
    #[test]
    fn the_controller_state_maps_and_only_a_real_start_counts() {
        assert_eq!(LongState::of(false, true), LongState::Idle);
        assert_eq!(LongState::of(true, false), LongState::Running);
        assert_eq!(LongState::of(true, true), LongState::Stopping);
        assert!(start_went_out(true, true));
        assert!(!start_went_out(true, false), "refused / not read: no notice next to the failure beep");
        assert!(!start_went_out(false, true), "already running or starting before");
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

    /// Gate 20b v3 (optional): while the service is not up the retries wait, uncounted, for
    /// up to 5 minutes; then the five tries run.
    #[test]
    fn a_slow_boot_waits_for_the_service_before_spending_the_tries() {
        let t0 = Instant::now();
        let mut retry = RegisterRetry::failed_at(t0);
        let (mut waited, mut tries) = (0, 0);
        for step in 1..=60u32 {
            let now = t0 + step * T10;
            if !retry.due(now) {
                continue;
            }
            if retry.wait_for_service(now) {
                waited += 1;
            } else {
                tries += 1;
                retry.tried(false, now);
            }
        }
        assert_eq!((waited, tries), (29, 5), "10 s .. 290 s waited, then 5 tries");
        assert_eq!(retry, RegisterRetry::NONE);
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
