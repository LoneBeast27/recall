//! The session window's messages through `handle` / `dispatch` / `session_tick` with a fake
//! controller: a long recording that reports stopping after a few engine drains, a virtual clock.

use super::*;
use crate::record_session_end::{STOP_WAIT, WM_ENDSESSION, WM_QUERYENDSESSION, WM_WTSSESSION_CHANGE, WTS_SESSION_LOCK, WTS_SESSION_UNLOCK};

const MIN: Duration = Duration::from_secs(60);
const CANCEL_NOTICE: &str = "notice Shutdown cancelled / Recording continues in a new file / shutdown_cancelled";

struct Fake {
    t0: Instant,
    clock: Duration,
    recording: bool,
    /// A `record_stop` is on its way (any sender).
    stopping: bool,
    /// Engine drains until a sent stop is reported (`None`: never).
    stops_after: Option<u32>,
    drains: u32,
    /// Whether a start is taken.
    starts: bool,
    /// A replay save is being written.
    saving: bool,
    calls: Vec<String>,
    state: SessionState,
}

impl Fake {
    fn new(recording: bool) -> Self {
        Fake { t0: Instant::now(), clock: Duration::ZERO, recording, stopping: false, stops_after: Some(3), drains: 0, starts: true, saving: false, calls: Vec::new(), state: SessionState::default() }
    }

    fn at(&self, d: Duration) -> Instant {
        self.t0 + d
    }
}

impl SessionHost for Fake {
    fn long_recording(&self) -> bool {
        self.recording
    }
    fn long_state(&self) -> LongState {
        LongState::of(self.recording, self.stopping)
    }
    fn send_record_stop_now(&mut self) {
        self.stopping = true;
        self.calls.push("stop".into());
    }
    fn start_long_recording(&mut self) -> bool {
        self.calls.push("start".into());
        self.recording = self.starts;
        self.starts
    }
    fn drain_engine(&mut self) {
        self.drains += 1;
        if self.stopping && self.stops_after.is_some_and(|n| self.drains >= n) {
            self.recording = false;
            self.stopping = false;
        }
    }
    fn block_shutdown(&mut self, reason: &str) {
        self.calls.push(format!("block {reason}"));
    }
    fn unblock_shutdown(&mut self) {
        self.calls.push("unblock".into());
    }
    fn session_shutdown(&mut self, grace: Duration) {
        self.calls.push(format!("shutdown {}s", grace.as_secs()));
    }
    fn saves_in_progress(&mut self) -> bool {
        self.saving
    }
    fn finish_saves(&mut self) {
        self.calls.push("finish saves".into());
        self.saving = false;
    }
    fn tell_engine(&mut self, line: &str) {
        self.calls.push(format!("engine {line}"));
    }
    fn session_notice(&mut self, title: &str, detail: &str, app_event: &str) {
        let kind: serde_json::Value = serde_json::from_str(app_event).unwrap();
        self.calls.push(format!("notice {title} / {detail} / {}", kind["type"].as_str().unwrap()));
    }
    fn clock(&self) -> Instant {
        self.t0 + self.clock
    }
    fn pause_for(&mut self, d: Duration) {
        self.clock += d;
    }
    fn session_state(&mut self) -> &mut SessionState {
        &mut self.state
    }
}

/// The window procedure's path: `handle` with the fake lent, as `drain_messages` lends the
/// controller.
fn proc(lender: &Lender<Fake>, fake: &mut Fake, message: u32, wparam: usize, pending: &Mutex<Vec<SessionMsg>>) -> Option<isize> {
    lender.lend(fake, || handle(lender, message, wparam, pending))
}

/// Gate 20b P1: a shutdown another app cancels (`WM_ENDSESSION`, fEnding FALSE) starts the
/// long recording the query stopped again, with the notice; nothing shuts down.
#[test]
fn a_cancelled_shutdown_restarts_the_long_recording_with_a_notice() {
    let (lender, pending) = (Lender::new(), Mutex::new(Vec::new()));
    let mut fake = Fake::new(true);
    assert_eq!(proc(&lender, &mut fake, WM_QUERYENDSESSION, 0, &pending), Some(1), "never refused");
    assert_eq!(fake.calls, ["block Saving your recording", "stop", "unblock"]);
    assert!(!fake.recording, "saved before the answer");
    assert_eq!(proc(&lender, &mut fake, WM_ENDSESSION, 0, &pending), Some(0));
    assert_eq!(fake.calls[3..], ["start", CANCEL_NOTICE]);
    assert!(fake.recording && pending.lock().unwrap().is_empty());
}

/// Gate 20b v3 P1: a stop the user (or a pause / retarget / sleep) sent just before the query
/// is waited for, and a cancel does not undo it.
#[test]
fn a_stop_already_in_flight_at_the_query_is_saved_and_never_restarted() {
    let (lender, pending) = (Lender::new(), Mutex::new(Vec::new()));
    let mut fake = Fake::new(true);
    fake.stopping = true; // the user's record_stop, recording_stopped not in yet
    proc(&lender, &mut fake, WM_QUERYENDSESSION, 0, &pending);
    assert_eq!(fake.calls, ["block Saving your recording", "unblock"], "no second stop");
    assert!(!fake.recording, "the user's stop was waited for");
    proc(&lender, &mut fake, WM_ENDSESSION, 0, &pending);
    assert_eq!(fake.calls.len(), 2, "no restart: {:?}", fake.calls);
}

/// Gate 20b v3 P2-1: a query that finds nothing recording clears an arm left by an earlier one.
#[test]
fn a_stale_arm_is_cleared_by_a_query_that_finds_nothing_recording() {
    let (lender, pending) = (Lender::new(), Mutex::new(Vec::new()));
    let mut fake = Fake::new(true);
    proc(&lender, &mut fake, WM_QUERYENDSESSION, 0, &pending); // armed, recording saved
    proc(&lender, &mut fake, WM_QUERYENDSESSION, 0, &pending); // a later query: nothing recording
    proc(&lender, &mut fake, WM_ENDSESSION, 0, &pending);
    assert!(!fake.calls.contains(&"start".to_string()), "{:?}", fake.calls);
}

/// Gate 20b v3 P2-2: a restart the engine does not take shows no "Recording continues".
#[test]
fn a_restart_that_did_not_go_out_shows_no_notice() {
    let (lender, pending) = (Lender::new(), Mutex::new(Vec::new()));
    let mut fake = Fake::new(true);
    fake.starts = false;
    proc(&lender, &mut fake, WM_QUERYENDSESSION, 0, &pending);
    proc(&lender, &mut fake, WM_ENDSESSION, 0, &pending);
    assert_eq!(fake.calls.last().unwrap(), "start");
}

#[test]
fn a_real_session_end_shuts_down_and_a_cancel_with_nothing_stopped_does_nothing() {
    let (lender, pending) = (Lender::new(), Mutex::new(Vec::new()));
    let mut fake = Fake::new(true);
    proc(&lender, &mut fake, WM_QUERYENDSESSION, 0, &pending);
    proc(&lender, &mut fake, WM_ENDSESSION, 1, &pending);
    assert_eq!(fake.calls.last().unwrap(), "shutdown 3s");
    assert!(!fake.calls.contains(&"start".to_string()));

    let mut idle = Fake::new(false);
    proc(&lender, &mut idle, WM_QUERYENDSESSION, 0, &pending);
    proc(&lender, &mut idle, WM_ENDSESSION, 0, &pending);
    assert_eq!(idle.calls, Vec::<String>::new(), "no recording: no block, no stop, no restart");
}

/// Outside the pump's drain (nothing lent) or from a nested message (the controller in use)
/// nothing is done in place: the query is answered yes and queued, a cancel is queued, other
/// messages go to the default procedure.
#[test]
fn without_the_controller_the_window_queues_and_answers_yes() {
    let (lender, pending) = (Lender::<Fake>::new(), Mutex::new(Vec::new()));
    assert_eq!(handle(&lender, WM_QUERYENDSESSION, 0, &pending), Some(1));
    assert_eq!(handle(&lender, WM_ENDSESSION, 0, &pending), Some(0));
    assert_eq!(handle(&lender, WM_ENDSESSION, 1, &pending), Some(0));
    assert_eq!(handle(&lender, WM_WTSSESSION_CHANGE, WTS_SESSION_LOCK, &pending), Some(0));
    assert_eq!(handle(&lender, 0x0010, 0, &pending), None, "WM_CLOSE: default procedure");
    assert_eq!(*pending.lock().unwrap(), [SessionMsg::QueryEnd, SessionMsg::End { ending: false }, SessionMsg::Lock]);

    let mut fake = Fake::new(true);
    let nested = lender.lend(&mut fake, || lender.with(|outer| {
        assert!(outer.is_some(), "the drain lends it");
        lender.with(|inner| inner.is_none())
    }));
    assert!(nested, "a nested message cannot reach the controller twice");
    assert!(lender.with(|after| after.is_none()), "returned when the drain ends");
}

/// A late query (answered outside the drain) stops the recording on the next pump pass, and
/// a late cancel restarts it.
#[test]
fn the_pump_handles_a_late_query_and_a_late_cancel() {
    let mut fake = Fake::new(true);
    let t = fake.at(MIN);
    session_tick(&mut fake, vec![SessionMsg::QueryEnd], t);
    assert_eq!(fake.calls, ["stop"]);
    session_tick(&mut fake, vec![SessionMsg::End { ending: false }], t);
    assert_eq!(fake.calls[1..], ["start", CANCEL_NOTICE]);
}

#[test]
fn the_pump_turns_lock_notices_into_engine_lines_and_one_unlock_notice() {
    let (lender, pending) = (Lender::<Fake>::new(), Mutex::new(Vec::new()));
    let mut fake = Fake::new(true);
    handle(&lender, WM_WTSSESSION_CHANGE, WTS_SESSION_LOCK, &pending);
    let t0 = fake.at(Duration::ZERO);
    session_tick(&mut fake, std::mem::take(&mut *pending.lock().unwrap()), t0);
    assert_eq!(fake.calls, [r#"engine {"cmd":"session_locked","locked":true}"#]);
    handle(&lender, WM_WTSSESSION_CHANGE, WTS_SESSION_UNLOCK, &pending);
    session_tick(&mut fake, std::mem::take(&mut *pending.lock().unwrap()), t0 + 12 * MIN);
    assert_eq!(fake.calls[1..], [r#"engine {"cmd":"session_locked","locked":false}"#, "notice Recording kept going while locked / 12:00 locked / recording_locked"]);
}

/// Gate 20b P2-d: a recording that stopped during the lock is announced as stopped.
#[test]
fn a_recording_that_stopped_during_the_lock_is_not_said_to_have_kept_going() {
    let mut fake = Fake::new(true);
    let t0 = fake.at(Duration::ZERO);
    session_tick(&mut fake, vec![SessionMsg::Lock], t0);
    fake.recording = false; // the disk filled while locked
    session_tick(&mut fake, vec![SessionMsg::Unlock], t0 + 5 * MIN);
    assert_eq!(fake.calls.last().unwrap(), "notice Recording stopped while locked / 5:00 locked / recording_stopped_locked");
}

/// Gate 20b v3 P2-4: a cancelled shutdown on the lock screen stops and restarts the recording
/// inside one lock, with no pump pass between: not "kept going" for the whole lock.
#[test]
fn a_stop_and_restart_within_one_lock_ran_for_part_of_it() {
    let (lender, pending) = (Lender::new(), Mutex::new(Vec::new()));
    let mut fake = Fake::new(true);
    let t0 = fake.at(Duration::ZERO);
    session_tick(&mut fake, vec![SessionMsg::Lock], t0);
    proc(&lender, &mut fake, WM_QUERYENDSESSION, 0, &pending);
    proc(&lender, &mut fake, WM_ENDSESSION, 0, &pending);
    assert!(fake.recording);
    session_tick(&mut fake, vec![SessionMsg::Unlock], t0 + 12 * MIN);
    assert_eq!(fake.calls.last().unwrap(), "notice Recording ran for part of the lock / 12:00 locked / recording_partly_locked");
}

#[test]
fn a_fresh_engine_learns_the_lock_only_while_locked() {
    let mut fake = Fake::new(false);
    reapply_lock(&mut fake);
    assert!(fake.calls.is_empty());
    let t0 = fake.at(Duration::ZERO);
    session_tick(&mut fake, vec![SessionMsg::Lock], t0);
    fake.calls.clear();
    reapply_lock(&mut fake);
    assert_eq!(fake.calls, [r#"engine {"cmd":"session_locked","locked":true}"#]);
    session_tick(&mut fake, vec![SessionMsg::Unlock], t0 + MIN);
    fake.calls.clear();
    reapply_lock(&mut fake);
    assert!(fake.calls.is_empty());
}

#[test]
fn the_stop_wait_ends_when_the_recording_reports_stopping_or_after_3_s() {
    let mut fake = Fake::new(true);
    fake.send_record_stop_now();
    assert!(wait_recording_stopped(&mut fake, STOP_WAIT));
    assert_eq!(fake.drains, 3);
    assert_eq!(fake.clock, Duration::from_millis(40), "two pauses before the third drain saw it");

    let mut stuck = Fake::new(true);
    stuck.stops_after = None;
    stuck.send_record_stop_now();
    assert!(!wait_recording_stopped(&mut stuck, STOP_WAIT));
    assert!(stuck.clock >= STOP_WAIT && stuck.clock < STOP_WAIT + Duration::from_millis(40), "{:?}", stuck.clock);
}

/// F-overlay-1 (P1): "Sign out anyway" or a forced update restart (`WM_ENDSESSION` TRUE) while
/// a replay save is written waits it out (bounded) before the 3 s shutdown, in the handler.
#[test]
fn a_real_session_end_during_a_save_finishes_it_before_the_shutdown() {
    let (lender, pending) = (Lender::new(), Mutex::new(Vec::new()));
    let mut host = Fake::new(false);
    host.saving = true;
    assert_eq!(proc(&lender, &mut host, WM_ENDSESSION, 1, &pending), Some(0));
    let finish = host.calls.iter().position(|c| c == "finish saves").expect("the quit wait runs");
    let shut = host.calls.iter().position(|c| c.starts_with("shutdown")).unwrap();
    assert!(finish < shut, "{:?}", host.calls);
    let mut idle = Fake::new(false);
    end(&mut idle, true);
    assert_eq!(idle.calls, ["shutdown 3s"], "no save: straight to the shutdown");
}

/// F-overlay-1: the same for a session end handled late, from the pump (`session_tick`).
#[test]
fn a_late_session_end_during_a_save_finishes_it_before_the_shutdown() {
    let mut host = Fake::new(false);
    host.saving = true;
    let now = host.at(Duration::ZERO);
    session_tick(&mut host, vec![SessionMsg::End { ending: true }], now);
    assert_eq!(host.calls, ["finish saves", "shutdown 3s"]);
    let mut cancelled = Fake::new(false);
    cancelled.saving = true;
    session_tick(&mut cancelled, vec![SessionMsg::End { ending: false }], now);
    assert!(cancelled.calls.is_empty(), "a cancel waits for nothing: {:?}", cancelled.calls);
}
