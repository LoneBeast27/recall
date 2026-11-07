//! The session window's messages through `dispatch` / `session_tick` with a fake controller:
//! a long recording that reports stopping after a few engine drains, a virtual clock.

use super::*;
use crate::record_session_end::{STOP_WAIT, WM_ENDSESSION, WM_QUERYENDSESSION, WM_WTSSESSION_CHANGE, WTS_SESSION_LOCK, WTS_SESSION_UNLOCK};

const MIN: Duration = Duration::from_secs(60);

struct Fake {
    t0: Instant,
    clock: Duration,
    recording: bool,
    /// Engine drains until a sent stop is reported (`None`: never).
    stops_after: Option<u32>,
    stop_sent: bool,
    drains: u32,
    /// Whether a start is taken.
    starts: bool,
    calls: Vec<String>,
    state: SessionState,
}

impl Fake {
    fn new(recording: bool) -> Self {
        Fake { t0: Instant::now(), clock: Duration::ZERO, recording, stops_after: Some(3), stop_sent: false, drains: 0, starts: true, calls: Vec::new(), state: SessionState::default() }
    }

    fn at(&self, d: Duration) -> Instant {
        self.t0 + d
    }
}

impl SessionHost for Fake {
    fn recording(&self) -> bool {
        self.recording
    }
    fn send_stop(&mut self) {
        self.stop_sent = true;
        self.calls.push("stop".into());
    }
    fn start_recording(&mut self) -> bool {
        self.calls.push("start".into());
        self.recording = self.starts;
        self.starts
    }
    fn drain_engine(&mut self) {
        self.drains += 1;
        if self.stop_sent && self.stops_after.is_some_and(|n| self.drains >= n) {
            self.recording = false;
        }
    }
    fn block(&mut self, reason: &str) {
        self.calls.push(format!("block {reason}"));
    }
    fn unblock(&mut self) {
        self.calls.push("unblock".into());
    }
    fn shutdown(&mut self, grace: Duration) {
        self.calls.push(format!("shutdown {}s", grace.as_secs()));
    }
    fn tell_engine(&mut self, line: &str) {
        self.calls.push(format!("engine {line}"));
    }
    fn notice(&mut self, title: &str, detail: &str, app_event: &str) {
        let kind: serde_json::Value = serde_json::from_str(app_event).unwrap();
        self.calls.push(format!("notice {title} / {detail} / {}", kind["type"].as_str().unwrap()));
    }
    fn now(&self) -> Instant {
        self.t0 + self.clock
    }
    fn pause(&mut self, d: Duration) {
        self.clock += d;
    }
    fn state(&mut self) -> &mut SessionState {
        &mut self.state
    }
}

/// Gate 20b P1: a shutdown another app cancels (`WM_ENDSESSION`, fEnding FALSE) starts the
/// long recording the query stopped again, with the notice; nothing shuts down.
#[test]
fn a_cancelled_shutdown_restarts_the_long_recording_with_a_notice() {
    let mut fake = Fake::new(true);
    let mut queue = Vec::new();
    assert_eq!(dispatch(Some(&mut fake), WM_QUERYENDSESSION, 0, &mut queue), Some(1), "never refused");
    assert_eq!(fake.calls, ["block Saving your recording", "stop", "unblock"]);
    assert!(!fake.recording, "saved before the answer");
    assert_eq!(dispatch(Some(&mut fake), WM_ENDSESSION, 0, &mut queue), Some(0));
    assert_eq!(fake.calls[3..], ["start", "notice Shutdown cancelled / Recording continues in a new file / shutdown_cancelled"]);
    assert!(fake.recording && queue.is_empty());
}

#[test]
fn a_real_session_end_shuts_down_and_a_cancel_with_nothing_stopped_does_nothing() {
    let mut fake = Fake::new(true);
    let mut queue = Vec::new();
    dispatch(Some(&mut fake), WM_QUERYENDSESSION, 0, &mut queue);
    dispatch(Some(&mut fake), WM_ENDSESSION, 1, &mut queue);
    assert_eq!(fake.calls.last().unwrap(), "shutdown 3s");
    assert!(!fake.calls.contains(&"start".to_string()));

    let mut idle = Fake::new(false);
    dispatch(Some(&mut idle), WM_QUERYENDSESSION, 0, &mut queue);
    dispatch(Some(&mut idle), WM_ENDSESSION, 0, &mut queue);
    assert_eq!(idle.calls, Vec::<String>::new(), "no recording: no block, no stop, no restart");
}

/// Outside the pump's drain nothing is done in place: the query is answered yes and queued,
/// a cancel is queued, other messages go to the default procedure.
#[test]
fn without_the_controller_the_window_queues_and_answers_yes() {
    let mut queue = Vec::new();
    assert_eq!(dispatch::<Fake>(None, WM_QUERYENDSESSION, 0, &mut queue), Some(1));
    assert_eq!(dispatch::<Fake>(None, WM_ENDSESSION, 0, &mut queue), Some(0));
    assert_eq!(dispatch::<Fake>(None, WM_ENDSESSION, 1, &mut queue), Some(0));
    assert_eq!(dispatch::<Fake>(None, WM_WTSSESSION_CHANGE, WTS_SESSION_LOCK, &mut queue), Some(0));
    assert_eq!(dispatch::<Fake>(None, 0x0010, 0, &mut queue), None, "WM_CLOSE: default procedure");
    assert_eq!(queue, [SessionMsg::QueryEnd, SessionMsg::End { ending: false }, SessionMsg::Lock]);
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
    assert_eq!(fake.calls[1..], ["start", "notice Shutdown cancelled / Recording continues in a new file / shutdown_cancelled"]);
}

#[test]
fn the_pump_turns_lock_notices_into_engine_lines_and_one_unlock_notice() {
    let mut fake = Fake::new(true);
    let mut queue = Vec::new();
    dispatch::<Fake>(None, WM_WTSSESSION_CHANGE, WTS_SESSION_LOCK, &mut queue);
    let t0 = fake.at(Duration::ZERO);
    session_tick(&mut fake, std::mem::take(&mut queue), t0);
    assert_eq!(fake.calls, [r#"engine {"cmd":"session_locked","locked":true}"#]);
    dispatch::<Fake>(None, WM_WTSSESSION_CHANGE, WTS_SESSION_UNLOCK, &mut queue);
    session_tick(&mut fake, queue, t0 + 12 * MIN);
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
    fake.send_stop();
    assert!(wait_recording_stopped(&mut fake, STOP_WAIT));
    assert_eq!(fake.drains, 3);
    assert_eq!(fake.clock, Duration::from_millis(40), "two pauses before the third drain saw it");

    let mut stuck = Fake::new(true);
    stuck.stops_after = None;
    stuck.send_stop();
    assert!(!wait_recording_stopped(&mut stuck, STOP_WAIT));
    assert!(stuck.clock >= STOP_WAIT && stuck.clock < STOP_WAIT + Duration::from_millis(40), "{:?}", stuck.clock);
}
