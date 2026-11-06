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
    /// The encoder's count while it records.
    frames: Option<u64>,
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
    fn encoded_frames(&self) -> Option<u64> {
        self.frames
    }
}

/// A game window in front, on the user's desktop with the display on.
const SHOWN: Screen = Screen { iconic: false, locked: false, display_off: false, game_in_front: true };
const MINIMIZED: Screen = Screen { iconic: true, game_in_front: false, ..SHOWN };
const LOCKED: Screen = Screen { locked: true, game_in_front: false, ..SHOWN };
const DISPLAY_OFF: Screen = Screen { display_off: true, ..SHOWN };
const BEHIND: Screen = Screen { game_in_front: false, ..SHOWN };

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

// A recording whose encoded-frame count stopped (`capture_stall` through `watch`).

/// A running capture recording a stuck count, `screen` for seconds 0..=to.
fn stalled_with(screen: Screen, to: u64) -> Fake {
    let t0 = Instant::now();
    let mut f = Fake { capture: true, start_ok: true, screen, frames: Some(900), ..Default::default() };
    f.backoff.started(t0);
    for s in 0..=to {
        watch(&mut f, t0 + secs(s));
    }
    f
}

#[test]
fn a_stalled_recording_in_front_gets_one_capture_restart() {
    let f = stalled_with(SHOWN, 15);
    assert_eq!((f.stops, f.starts), (1, 1));
}

#[test]
fn no_stall_restart_while_minimized_behind_a_window_locked_or_the_display_is_off() {
    for (why, screen) in [("minimized", MINIMIZED), ("behind", BEHIND), ("locked", LOCKED), ("display off", DISPLAY_OFF)] {
        let f = stalled_with(screen, 60);
        assert_eq!((f.stops, f.starts), (0, 0), "{why}: 60 s");
    }
}

#[test]
fn a_monitor_capture_or_an_encoder_not_recording_never_stalls() {
    assert_eq!(stalled_with(Screen { game_in_front: false, ..SHOWN }, 60).stops, 0, "monitor capture");
    let t0 = Instant::now();
    let mut f = Fake { capture: true, screen: SHOWN, frames: None, ..Default::default() };
    for s in 0..=60 {
        watch(&mut f, t0 + secs(s));
    }
    assert_eq!(f.stops, 0, "starting / recovering");
}
