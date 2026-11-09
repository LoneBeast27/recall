//! What the hidden session window's messages do (U6 sign-out / shutdown, U8 lock), written
//! against [`SessionHost`] so the tests drive the same code the window procedure runs with a
//! fake controller: `record_session_win` implements the host on the real one and only adds
//! the Win32 calls. [`Lender`] + [`handle`] are the window procedure's body.

use std::cell::Cell;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use crate::log;
use crate::record_lock::{app_event_json, engine_line, notice_detail, notice_title, LockAction, LockWatch};
use crate::record_session_end::{
    cancel_event_json, EndAction, LongState, SessionEnd, SessionMsg, CANCEL_DETAIL, CANCEL_TITLE, QUERY_ANSWER,
};

/// Session-end and lock state the controller keeps.
#[derive(Debug, Default)]
pub struct SessionState {
    pub lock: LockWatch,
    pub end: SessionEnd,
}

/// The controller as the session messages see it. Names differ from the controller's own
/// methods on purpose (no shadowing either way).
pub trait SessionHost {
    /// For the lock: a long recording is running (`EngineActivity::is_recording`).
    fn long_recording(&self) -> bool;
    /// For a session end: running or starting, and whether a stop is already on its way.
    fn long_state(&self) -> LongState;
    /// Send the engine `record_stop` (marks the stop in flight).
    fn send_record_stop_now(&mut self);
    /// Start a long recording; true only when this start went out (not refused, not sent to
    /// an engine that is not reading, not one already running).
    fn start_long_recording(&mut self) -> bool;
    /// Take the engine's events so far.
    fn drain_engine(&mut self);
    fn block_shutdown(&mut self, reason: &str);
    fn unblock_shutdown(&mut self);
    /// The controller's clean shutdown, the engine quit capped at `grace`.
    fn session_shutdown(&mut self, grace: Duration);
    /// A replay save is being written (the quit's wait would hold for it).
    fn saves_in_progress(&mut self) -> bool;
    /// Wait for the saves being written, bounded by the quit's wait; a save it gives up on
    /// is reported "Recall was closed during the save". Returns once the shutdown may go on.
    fn finish_saves(&mut self);
    /// A line for the engine (not a user action: a failure only loses it).
    fn tell_engine(&mut self, line: &str);
    /// A notice on the island and, through `app_event`, a toast in the app.
    fn session_notice(&mut self, title: &str, detail: &str, app_event: &str);
    fn clock(&self) -> Instant;
    fn pause_for(&mut self, d: Duration);
    fn session_state(&mut self) -> &mut SessionState;
}

/// The controller lent to the window procedure for one message drain. Taken out while in
/// use, so a nested message (a send from inside a handler) finds none.
pub struct Lender<T>(Cell<*mut T>);

impl<T> Lender<T> {
    pub const fn new() -> Self {
        Lender(Cell::new(std::ptr::null_mut()))
    }

    /// Lend `t` while `f` runs.
    pub fn lend<R>(&self, t: &mut T, f: impl FnOnce() -> R) -> R {
        let before = self.0.replace(t);
        let out = f();
        self.0.set(before);
        out
    }

    /// Run `f` on what is lent; `None` when nothing is (or it is in use already).
    pub fn with<R>(&self, f: impl FnOnce(Option<&mut T>) -> R) -> R {
        let ptr = self.0.replace(std::ptr::null_mut());
        if ptr.is_null() {
            return f(None);
        }
        // SAFETY: set by `lend` from a `&mut T` that is not used until `lend` returns;
        // taken out while `f` runs, so nothing else can reach it.
        let out = f(Some(unsafe { &mut *ptr }));
        self.0.set(ptr);
        out
    }
}

/// The window procedure: `None` = not a session message (the default procedure answers it).
pub fn handle<H: SessionHost>(lender: &Lender<H>, message: u32, wparam: usize, pending: &Mutex<Vec<SessionMsg>>) -> Option<isize> {
    let mut queued = Vec::new();
    let answer = lender.with(|host| dispatch(host, message, wparam, &mut queued));
    pending.lock().unwrap_or_else(|p| p.into_inner()).extend(queued);
    answer
}

/// `message` / `wparam` with the controller lent (`Some`) or not. What cannot be done here
/// goes to `queue`, for [`session_tick`].
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
    let long = host.long_state();
    log!("record: the session is ending (long recording: {long:?})");
    let actions = host.session_state().end.on_query_end(long);
    run(host, actions);
}

/// `WM_ENDSESSION`: `ending` false = the shutdown was cancelled.
pub fn end<H: SessionHost>(host: &mut H, ending: bool) {
    let saving = ending && host.saves_in_progress();
    let actions = host.session_state().end.on_end(ending, saving);
    run(host, actions);
}

fn run<H: SessionHost>(host: &mut H, actions: Vec<EndAction>) {
    for action in actions {
        match action {
            EndAction::Block(reason) => host.block_shutdown(reason),
            EndAction::StopRecording => host.send_record_stop_now(),
            EndAction::WaitStopped(wait) => {
                wait_recording_stopped(host, wait);
            }
            EndAction::Unblock => host.unblock_shutdown(),
            EndAction::Shutdown(grace) => {
                log!("record: the session is ending -- shutting down");
                host.session_shutdown(grace);
            }
            EndAction::Restart => restart(host),
            EndAction::FinishSaves => {
                log!("record: the session is ending during a save -- finishing it before the shutdown");
                host.finish_saves();
            }
        }
    }
}

fn restart<H: SessionHost>(host: &mut H) {
    host.session_state().lock.recording_seen(false); // a lock around this is not "kept going"
    if host.start_long_recording() {
        log!("record: shutdown cancelled -- the long recording continues in a new file");
        host.session_notice(CANCEL_TITLE, CANCEL_DETAIL, &cancel_event_json());
    } else {
        log!("record: shutdown cancelled -- the long recording could not start again");
    }
}

/// Take engine events until the long recording reports stopping, or `wait` runs out.
/// True when it stopped.
pub fn wait_recording_stopped<H: SessionHost>(host: &mut H, wait: Duration) -> bool {
    let started = host.clock();
    loop {
        host.drain_engine();
        let now = host.clock();
        if host.long_state() == LongState::Idle {
            log!("record: session end -- long recording saved in {:?}", now - started);
            return true;
        }
        if now - started >= wait {
            log!("record: session end -- no recording_stopped in {wait:?}; the .part is recovered on the next start");
            return false;
        }
        host.pause_for(Duration::from_millis(20));
    }
}

/// Pump step: the lock notices, and session-end messages the window could not handle in
/// place, since the last pass.
pub fn session_tick<H: SessionHost>(host: &mut H, pending: Vec<SessionMsg>, now: Instant) {
    let recording = host.long_recording();
    host.session_state().lock.recording_seen(recording);
    for msg in pending {
        let actions = match msg {
            SessionMsg::Lock => host.session_state().lock.on_lock(now, recording),
            SessionMsg::Unlock => host.session_state().lock.on_unlock(now, recording),
            SessionMsg::QueryEnd => {
                let long = host.long_state();
                log!("record: the session is ending (late; long recording: {long:?})");
                let actions = host.session_state().end.on_late_query_end(long);
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
        LockAction::Notice { span, outcome } => {
            let (title, detail) = (notice_title(outcome), notice_detail(span));
            log!("record: unlocked -- {title} ({detail})");
            host.session_notice(title, &detail, &app_event_json(span, outcome));
        }
    }
}

/// A fresh engine learns that the session is locked (it counts from now).
pub fn reapply_lock<H: SessionHost>(host: &mut H) {
    if host.session_state().lock.is_locked() {
        host.tell_engine(&engine_line(true));
    }
}

#[cfg(test)]
#[path = "record_session_host_tests.rs"]
mod tests;
