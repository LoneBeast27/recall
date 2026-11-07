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
//! The recording stops at the query, so a shutdown another app cancels still ends it (the
//! file is saved; a new recording is one shortcut away).
//!
//! This module only decides; `record_session_win` owns the hidden window that receives the
//! messages (and the session lock notices, `record_lock`) and applies the actions.

use std::time::Duration;

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
}

/// `WM_QUERYENDSESSION`: with a long recording running, save it before answering.
pub fn on_query_end(recording: bool) -> Vec<EndAction> {
    if !recording {
        return Vec::new();
    }
    vec![EndAction::Block(BLOCK_REASON), EndAction::StopRecording, EndAction::WaitStopped(STOP_WAIT), EndAction::Unblock]
}

/// `WM_ENDSESSION`: shut down cleanly when the session really ends.
pub fn on_end(ending: bool) -> Vec<EndAction> {
    if ending { vec![EndAction::Shutdown(SESSION_END_GRACE)] } else { Vec::new() }
}

/// The answer to `WM_QUERYENDSESSION`: always TRUE (never hold up a shutdown).
pub const QUERY_ANSWER: isize = 1;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_shutdown_during_a_long_recording_saves_it_first() {
        assert_eq!(
            on_query_end(true),
            vec![EndAction::Block("Saving your recording"), EndAction::StopRecording, EndAction::WaitStopped(Duration::from_secs(3)), EndAction::Unblock]
        );
        assert_eq!(QUERY_ANSWER, 1, "the shutdown is never refused");
    }

    #[test]
    fn a_shutdown_with_no_recording_does_nothing_until_the_session_ends() {
        assert_eq!(on_query_end(false), vec![]);
        assert_eq!(on_end(false), vec![], "a cancelled shutdown");
        assert_eq!(on_end(true), vec![EndAction::Shutdown(SESSION_END_GRACE)]);
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
