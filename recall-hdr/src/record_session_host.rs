//! What the hidden session window's messages do (U6 sign-out / shutdown, U8 lock), written
//! against [`SessionHost`] so the tests drive the same code the window procedure runs with a
//! fake controller: `record_session_win` implements the host on the real one and only adds
//! the Win32 calls.

use std::time::{Duration, Instant};

use crate::log;
use crate::record_lock::{app_event_json, engine_line, notice_detail, notice_title, LockAction, LockWatch};
use crate::record_session_end::{
    cancel_event_json, EndAction, SessionEnd, SessionMsg, CANCEL_DETAIL, CANCEL_TITLE, QUERY_ANSWER,
};

/// Session-end and lock state the controller keeps.
#[derive(Debug, Default)]
pub struct SessionState {
    pub lock: LockWatch,
    pub end: SessionEnd,
}

/// The controller as the session messages see it.
pub trait SessionHost {
    /// A long recording is running or starting.
    fn recording(&self) -> bool;
    /// Send the engine `record_stop`.
    fn send_stop(&mut self);
    /// Start a long recording; true when it is on its way (not refused).
    fn start_recording(&mut self) -> bool;
    /// Take the engine's events so far.
    fn drain_engine(&mut self);
    fn block(&mut self, reason: &str);
    fn unblock(&mut self);
    /// The controller's clean shutdown, the engine quit capped at `grace`.
    fn shutdown(&mut self, grace: Duration);
    /// A line for the engine (not a user action: a failure only loses it).
    fn tell_engine(&mut self, line: &str);
    /// A notice on the island and, through `app_event`, a toast in the app.
    fn notice(&mut self, title: &str, detail: &str, app_event: &str);
    fn now(&self) -> Instant;
    fn pause(&mut self, d: Duration);
    fn state(&mut self) -> &mut SessionState;
}

/// The window procedure's part: `message` / `wparam` with the controller lent (`Some`) or not.
/// `None` = not a session message (the default procedure answers it). What cannot be done
/// here goes to `queue`, for [`session_tick`].
pub fn dispatch<H: SessionHost>(host: Option<&mut H>, message: u32, wparam: usize, queue: &mut Vec<SessionMsg>) -> Option<isize> {
    let msg = SessionMsg::classify(message, wparam)?;
    match (msg, host) {
        (SessionMsg::QueryEnd, Some(host)) => query_end(host),
        (SessionMsg::End { ending }, Some(host)) => end(host, ending),
        (SessionMsg::End { ending: true }, None) => log!("record: the session is ending outside the pump; the engine is left to Windows"),
        (msg, _) => queue.push(msg),
    }
    Some(if msg == SessionMsg::QueryEnd { QUERY_ANSWER } else { 0 })
}

/// `WM_QUERYENDSESSION` inside the pump's drain.
pub fn query_end<H: SessionHost>(host: &mut H) {
    let recording = host.recording();
    log!("record: the session is ending (long recording running: {recording})");
    let actions = host.state().end.on_query_end(recording);
    run(host, actions);
}

/// `WM_ENDSESSION`: `ending` false = the shutdown was cancelled.
pub fn end<H: SessionHost>(host: &mut H, ending: bool) {
    let actions = host.state().end.on_end(ending);
    run(host, actions);
}

fn run<H: SessionHost>(host: &mut H, actions: Vec<EndAction>) {
    for action in actions {
        match action {
            EndAction::Block(reason) => host.block(reason),
            EndAction::StopRecording => host.send_stop(),
            EndAction::WaitStopped(wait) => {
                wait_recording_stopped(host, wait);
            }
            EndAction::Unblock => host.unblock(),
            EndAction::Shutdown(grace) => {
                log!("record: the session is ending -- shutting down");
                host.shutdown(grace);
            }
            EndAction::Restart => {
                if host.start_recording() {
                    log!("record: shutdown cancelled -- the long recording continues in a new file");
                    host.notice(CANCEL_TITLE, CANCEL_DETAIL, &cancel_event_json());
                } else {
                    log!("record: shutdown cancelled -- the long recording could not start again");
                }
            }
        }
    }
}

/// Take engine events until the long recording reports stopping, or `wait` runs out.
/// True when it stopped.
pub fn wait_recording_stopped<H: SessionHost>(host: &mut H, wait: Duration) -> bool {
    let started = host.now();
    loop {
        host.drain_engine();
        let now = host.now();
        if !host.recording() {
            log!("record: session end -- long recording saved in {:?}", now - started);
            return true;
        }
        if now - started >= wait {
            log!("record: session end -- no recording_stopped in {wait:?}; the .part is recovered on the next start");
            return false;
        }
        host.pause(Duration::from_millis(20));
    }
}

/// Pump step: the lock notices, and session-end messages the window could not handle in
/// place, since the last pass.
pub fn session_tick<H: SessionHost>(host: &mut H, pending: Vec<SessionMsg>, now: Instant) {
    let recording = host.recording();
    host.state().lock.recording_seen(recording);
    for msg in pending {
        let actions = match msg {
            SessionMsg::Lock => host.state().lock.on_lock(now, recording),
            SessionMsg::Unlock => host.state().lock.on_unlock(now, recording),
            SessionMsg::QueryEnd => {
                if recording {
                    log!("record: the session is ending -- stopping the long recording (late)");
                }
                let actions = host.state().end.on_late_query_end(recording);
                run(host, actions);
                continue;
            }
            SessionMsg::End { ending } => {
                end(host, ending);
                continue;
            }
        };
        log!("record: session {msg:?} (long recording running: {recording})");
        for action in actions {
            apply_lock_action(host, action);
        }
    }
}

fn apply_lock_action<H: SessionHost>(host: &mut H, action: LockAction) {
    match action {
        LockAction::TellEngine(locked) => host.tell_engine(&engine_line(locked)),
        LockAction::Notice { span, running } => {
            let (title, detail) = (notice_title(running), notice_detail(span));
            log!("record: unlocked -- {title} ({detail})");
            host.notice(title, &detail, &app_event_json(span, running));
        }
    }
}

/// A fresh engine learns that the session is locked (it counts from now).
pub fn reapply_lock<H: SessionHost>(host: &mut H) {
    if host.state().lock.is_locked() {
        host.tell_engine(&engine_line(true));
    }
}

#[cfg(test)]
#[path = "record_session_host_tests.rs"]
mod tests;
