use super::*;

fn secs(s: u64) -> Duration {
    Duration::from_secs(s)
}

#[test]
fn a_failed_start_is_retried_after_1_2_5_10_then_every_10_seconds() {
    let t0 = Instant::now();
    let mut b = Backoff::default();
    let mut now = t0;
    let mut waits = Vec::new();
    for _ in 0..6 {
        let wait = b.start_failed(now);
        waits.push(wait.as_secs());
        assert_eq!(b.step(now + wait - Duration::from_millis(1), false, false), Step::Idle, "not before the wait");
        assert_eq!(b.step(now + wait, false, false), Step::Start);
        now += wait;
    }
    assert_eq!(waits, [1, 2, 5, 10, 10, 10]);
}

#[test]
fn nothing_is_tried_before_any_start_or_while_a_capture_runs() {
    let t0 = Instant::now();
    let mut b = Backoff::default();
    assert_eq!(b.step(t0 + secs(100), false, false), Step::Idle, "never started: start_capture's own call");
    b.started(t0);
    assert_eq!(b.step(t0 + secs(100), true, false), Step::Idle);
}

#[test]
fn a_dead_capture_thread_is_reaped_and_restarted_with_backoff() {
    let t0 = Instant::now();
    let mut b = Backoff::default();
    b.started(t0);
    assert_eq!(b.step(t0 + secs(3), true, true), Step::Reap);
    assert_eq!(b.died(t0 + secs(3)), secs(1));
    assert_eq!(b.step(t0 + secs(4), false, false), Step::Start);
    // Dies again right after the restart: the wait grows.
    b.started(t0 + secs(4));
    assert_eq!(b.died(t0 + secs(5)), secs(2));
}

#[test]
fn a_capture_that_ran_stable_starts_the_backoff_over() {
    let t0 = Instant::now();
    let mut b = Backoff::default();
    for i in 0..4 {
        b.start_failed(t0 + secs(i));
    }
    b.started(t0 + secs(10));
    assert_eq!(b.died(t0 + secs(10) + STABLE), secs(1));
}

// `watch` (App::watch_capture's body) over a fake slot.

/// A capture slot whose starts succeed or fail as told, and whose screen is `screen`.
#[derive(Default)]
struct Fake {
    backoff: Backoff,
    capture: bool,
    finished: bool,
    start_ok: bool,
    screen: Screen,
    starts: u32,
    stops: u32,
    /// A pending `restart_reason` (taken when read).
    reason: Option<&'static str>,
}

impl CaptureSlot for Fake {
    fn backoff(&mut self) -> &mut Backoff {
        &mut self.backoff
    }
    fn held(&self) -> (bool, bool) {
        (self.capture, self.capture && self.finished)
    }
    fn stop(&mut self) -> Option<String> {
        self.stops += 1;
        self.capture = false;
        self.finished = false;
        None
    }
    fn try_start(&mut self) -> Result<(), String> {
        self.starts += 1;
        self.capture = self.start_ok;
        if self.start_ok { Ok(()) } else { Err("no frames source".into()) }
    }
    fn screen(&self) -> Screen {
        self.screen
    }
    fn restart_reason(&mut self) -> Option<&'static str> {
        self.reason.take()
    }
}

/// The game window shown, on the user's desktop with the display on.
const SHOWN: Screen = Screen { iconic: false, locked: false, display_off: false };
const MINIMIZED: Screen = Screen { iconic: true, ..SHOWN };
const LOCKED: Screen = Screen { locked: true, ..SHOWN };
const DISPLAY_OFF: Screen = Screen { display_off: true, ..SHOWN };

/// A capture whose thread ended at t0, then `screen` for seconds 0..=to.
fn died_then(screen: Screen, to: u64) -> (Fake, Instant) {
    let t0 = Instant::now();
    let mut f = Fake { capture: true, finished: true, start_ok: true, screen, ..Default::default() };
    f.backoff.started(t0 - secs(60));
    for s in 0..=to {
        watch(&mut f, t0 + secs(s));
    }
    (f, t0)
}

#[test]
fn a_dead_capture_is_not_reaped_or_restarted_while_minimized_locked_or_the_display_is_off() {
    for (why, screen) in [("minimized", MINIMIZED), ("locked", LOCKED), ("display off", DISPLAY_OFF)] {
        let (f, _) = died_then(screen, 60);
        assert_eq!((f.stops, f.starts), (0, 0), "{why}: 60 s");
    }
}

#[test]
fn a_dead_capture_on_screen_is_reaped_and_restarted_once() {
    let (f, _) = died_then(Screen::default(), 60);
    assert_eq!((f.stops, f.starts), (1, 1));
    assert!(f.capture);
}

#[test]
fn a_failed_start_waits_while_locked_and_retries_once_the_user_is_back() {
    let t0 = Instant::now();
    let mut f = Fake { screen: LOCKED, ..Default::default() };
    note_start(&mut f.backoff, Err("lock screen".into()), t0);
    for s in 0..=60 {
        watch(&mut f, t0 + secs(s));
    }
    assert_eq!(f.starts, 0, "locked: no tries");
    f.screen = Screen::default();
    f.start_ok = true;
    assert_eq!(watch(&mut f, t0 + secs(61)), Step::Start);
    assert_eq!(f.starts, 1);
    assert_eq!(watch(&mut f, t0 + secs(62)), Step::Idle, "running: left alone");
}

// restart_capture_if_needed (`restart_if_asked`): a restart asked with no capture held.

#[test]
fn a_restart_asked_with_no_capture_is_taken_so_the_next_start_is_not_torn_down() {
    let t0 = Instant::now();
    let mut f = Fake { start_ok: true, screen: SHOWN, ..Default::default() };
    note_start(&mut f.backoff, Err("device lost".into()), t0); // a 1 s wait runs
    f.reason = Some("(capture source changed)");
    restart_if_asked(&mut f, t0);
    assert_eq!(f.starts, 0, "the wait runs");
    for s in 1..=5 {
        watch(&mut f, t0 + secs(s));
        restart_if_asked(&mut f, t0 + secs(s));
    }
    assert_eq!((f.starts, f.stops), (1, 0), "one start, never stopped by the old request");
}

#[test]
fn a_restart_asked_with_no_capture_and_no_wait_starts_now() {
    let t0 = Instant::now();
    let mut f = Fake { start_ok: true, screen: SHOWN, reason: Some("(capture source changed)"), ..Default::default() };
    restart_if_asked(&mut f, t0);
    assert_eq!(watch(&mut f, t0), Step::Start);
    assert_eq!(f.starts, 1);
    assert!(f.capture);
}

#[test]
fn a_restart_asked_while_locked_waits_for_the_user() {
    let t0 = Instant::now();
    let mut f = Fake { capture: true, start_ok: true, screen: LOCKED, reason: Some("(capture source changed)"), ..Default::default() };
    for s in 0..=60 {
        restart_if_asked(&mut f, t0 + secs(s));
    }
    assert_eq!((f.stops, f.starts), (0, 0));
    f.screen = SHOWN;
    restart_if_asked(&mut f, t0 + secs(61));
    assert_eq!((f.stops, f.starts), (1, 1), "replaced once the user is back");
}

// No new frames on their own never restart anything (the encoder is VFR, WGC sends no
// repeats): only a capture that ended or was asked to change is replaced.

/// A running capture that delivers nothing, ticked once a second for `0..=to`.
fn quiet_for(screen: Screen, to: u64) -> Fake {
    let t0 = Instant::now();
    let mut f = Fake { capture: true, start_ok: true, screen, ..Default::default() };
    f.backoff.started(t0);
    for s in 0..=to {
        tick(&mut f, t0 + secs(s));
    }
    f
}

#[test]
fn a_static_foreground_game_for_5_minutes_is_never_restarted() {
    // A paused game, a menu, a load screen or a shader compile: the capture is alive.
    let f = quiet_for(SHOWN, 300);
    assert_eq!((f.stops, f.starts), (0, 0), "no restart on a still picture");
}

#[test]
fn a_dead_thread_or_a_closed_item_is_restarted_once() {
    // A handler error or GraphicsCaptureItem.Closed both end the capture thread.
    let t0 = Instant::now();
    let mut f = Fake { capture: true, start_ok: true, screen: SHOWN, ..Default::default() };
    f.backoff.started(t0 - secs(60));
    f.finished = true;
    for s in 0..=300 {
        tick(&mut f, t0 + secs(s));
    }
    assert_eq!((f.stops, f.starts), (1, 1));
}

// `tick`: one periodic tick opens at most one capture; `restart_if_asked` backs off.

#[test]
fn a_start_and_a_pending_request_in_one_tick_open_one_capture() {
    let t0 = Instant::now();
    let mut f = Fake { start_ok: true, screen: SHOWN, reason: Some("(capture source changed)"), ..Default::default() };
    note_start(&mut f.backoff, Err("device lost".into()), t0 - secs(1)); // the 1 s wait ends at t0
    assert_eq!(tick(&mut f, t0), Step::Start);
    assert_eq!((f.starts, f.stops), (1, 0), "one device, the request is taken");
    tick(&mut f, t0 + secs(1));
    assert_eq!((f.starts, f.stops), (1, 0), "and does not tear the new capture down");
}

#[test]
fn requests_in_a_row_replace_the_capture_on_a_1_2_5_10_s_backoff() {
    let t0 = Instant::now();
    let mut f = Fake { capture: true, start_ok: true, screen: SHOWN, ..Default::default() };
    let mut at = Vec::new();
    for s in 0..=40 {
        f.reason = Some("(capture source changed)"); // asked every second
        let before = f.starts;
        tick(&mut f, t0 + secs(s));
        if f.starts > before {
            at.push(s);
        }
    }
    assert_eq!(at, [0, 1, 3, 8, 18, 28, 38]);
}

#[test]
fn a_request_inside_the_backoff_is_kept_for_when_it_runs_out() {
    let t0 = Instant::now();
    let mut f = Fake { capture: true, start_ok: true, screen: SHOWN, reason: Some("(a)"), ..Default::default() };
    tick(&mut f, t0); // replaced, a 1 s wait starts
    f.reason = Some("(b)");
    tick(&mut f, t0 + Duration::from_millis(500));
    assert_eq!(f.starts, 1, "inside the wait");
    tick(&mut f, t0 + secs(1));
    assert_eq!(f.starts, 2, "kept, then honoured");
}
