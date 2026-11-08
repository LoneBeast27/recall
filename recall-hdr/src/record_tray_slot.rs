//! The recording tray, retried until it exists (audit G8). At sign-in the Run-key start can
//! reach `Shell_NotifyIcon` before Explorer's notification area is ready; a failed build used
//! to end the controller (exit 2), leaving nothing recording all session. Now the controller
//! records without a tray and the pump retries the build with backoff (2 s doubling to 60 s,
//! never giving up). Status and pause changes made meanwhile are applied once it exists.
//!
//! tray-icon 0.24 also returns Ok when `Shell_NotifyIcon(NIM_ADD)` fails (it waits for a
//! `TaskbarCreated` that never comes when Explorer was already up), so one backoff step after
//! a build the slot asks the shell for the icon's rect; with none it drops the tray and builds
//! it again, at most [`MAX_UNSHOWN`] times (R2-deep-startup-2). The status line also says when
//! the control pipe gave up, so a user whose app can't reach the recorder sees why (R2-3).

use std::time::{Duration, Instant};

use crate::log;
use crate::record_tray::{RecordingTray, RecordingTrayCmd};

const FIRST_RETRY: Duration = Duration::from_secs(2);
const MAX_RETRY: Duration = Duration::from_secs(60);
/// Rebuilds of a built tray the shell never showed; after these it is kept as it is (an icon
/// the shell holds but reports no rect for must not flicker all session).
const MAX_UNSHOWN: u32 = 3;
const LINK_DOWN_NOTE: &str = "control link unavailable (see log)";

/// What the slot needs from a tray (the real one, or a test double).
pub(crate) trait TrayUi {
    fn poll(&self) -> Vec<RecordingTrayCmd>;
    fn set_status(&mut self, line: &str);
    fn set_paused(&mut self, paused: bool);
    /// The shell holds the icon (tray-icon's `build` succeeds even when NIM_ADD failed).
    fn is_shown(&self) -> bool;
}

impl TrayUi for RecordingTray {
    fn poll(&self) -> Vec<RecordingTrayCmd> {
        RecordingTray::poll(self)
    }
    fn set_status(&mut self, line: &str) {
        RecordingTray::set_status(self, line)
    }
    fn set_paused(&mut self, paused: bool) {
        RecordingTray::set_paused(self, paused)
    }
    fn is_shown(&self) -> bool {
        RecordingTray::is_shown(self)
    }
}

type Build<T> = Box<dyn FnMut() -> Result<T, String>>;

/// The tray once built, plus the retry schedule and the state to show when it appears.
pub(crate) struct TraySlot<T: TrayUi = RecordingTray> {
    tray: Option<T>,
    build: Build<T>,
    next_try: Instant,
    delay: Duration,
    failures: u32,
    status: Option<String>,
    paused: bool,
    /// The shell showed the built tray (or it was kept after [`MAX_UNSHOWN`] rebuilds).
    confirmed: bool,
    unshown: u32,
    /// The control pipe gave up (`control::link_down`); the status line carries a note.
    link_down: fn() -> bool,
    link_noted: bool,
}

impl TraySlot<RecordingTray> {
    /// The real tray, first tried now.
    pub(crate) fn recording(now: Instant) -> Self {
        Self::new(Box::new(RecordingTray::build), now).with_link_check(crate::control::link_down)
    }
}

fn link_up() -> bool {
    false
}

impl<T: TrayUi> TraySlot<T> {
    /// Tries `build` once at `now`; later tries happen in [`TraySlot::poll_at`].
    pub(crate) fn new(build: Build<T>, now: Instant) -> Self {
        let mut slot = TraySlot {
            tray: None,
            build,
            next_try: now,
            delay: FIRST_RETRY,
            failures: 0,
            status: None,
            paused: false,
            confirmed: false,
            unshown: 0,
            link_down: link_up,
            link_noted: false,
        };
        slot.try_build(now);
        slot
    }

    /// Notes a dead control pipe on the status line while `link_down` says so.
    pub(crate) fn with_link_check(mut self, link_down: fn() -> bool) -> Self {
        self.link_down = link_down;
        self
    }

    pub(crate) fn is_built(&self) -> bool {
        self.tray.is_some()
    }

    /// The status line as shown: the pump's line, plus the dead-pipe note.
    fn shown_line(&self, line: &str) -> String {
        if self.link_noted { format!("{line} \u{b7} {LINK_DOWN_NOTE}") } else { line.to_string() }
    }

    fn try_build(&mut self, now: Instant) {
        if self.tray.is_some() || now < self.next_try {
            return;
        }
        match (self.build)() {
            Ok(mut tray) => {
                if self.failures > 0 {
                    log!("record: tray created after {} failed attempt(s)", self.failures);
                }
                if let Some(line) = &self.status {
                    tray.set_status(&self.shown_line(line));
                }
                tray.set_paused(self.paused);
                self.tray = Some(tray);
                // Ask the shell for the icon one backoff step from now (`check_shown`).
                self.confirmed = false;
                self.next_try = now + self.delay;
                self.delay = (self.delay * 2).min(MAX_RETRY);
            }
            Err(e) => {
                self.failures += 1;
                // Every early failure, then one line per ~10 minutes at the 60 s cap.
                if self.failures <= 5 || self.failures.is_multiple_of(10) {
                    log!("record: tray unavailable ({e}); recording continues, retrying in {} s (attempt {})", self.delay.as_secs(), self.failures);
                }
                self.next_try = now + self.delay;
                self.delay = (self.delay * 2).min(MAX_RETRY);
            }
        }
    }

    /// A built tray the shell does not show is dropped (removing its window) so `try_build`
    /// makes it again.
    fn check_shown(&mut self, now: Instant) {
        let Some(tray) = self.tray.as_ref() else { return };
        if self.confirmed || now < self.next_try {
            return;
        }
        if tray.is_shown() {
            self.confirmed = true;
            if self.unshown > 0 {
                log!("record: tray icon shown after {} rebuild(s)", self.unshown);
            }
        } else if self.unshown >= MAX_UNSHOWN {
            self.confirmed = true;
            log!("record: the shell still reports no tray icon after {} rebuilds; keeping it", self.unshown);
        } else {
            self.unshown += 1;
            log!("record: tray icon missing (the shell did not take it); building it again (rebuild {})", self.unshown);
            self.tray = None;
        }
    }

    /// The dead-pipe note follows `link_down` (the pipe thread can give up at any time).
    fn follow_link(&mut self) {
        let down = (self.link_down)();
        if down == self.link_noted {
            return;
        }
        self.link_noted = down;
        let Some(line) = self.status.as_deref() else { return };
        let shown = self.shown_line(line);
        if let Some(tray) = self.tray.as_mut() {
            tray.set_status(&shown);
        }
    }

    /// Retries a missing (or unshown) tray when due, then drains its commands.
    pub(crate) fn poll_at(&mut self, now: Instant) -> Vec<RecordingTrayCmd> {
        self.check_shown(now);
        self.try_build(now);
        self.follow_link();
        self.tray.as_ref().map(TrayUi::poll).unwrap_or_default()
    }

    pub(crate) fn poll(&mut self) -> Vec<RecordingTrayCmd> {
        self.poll_at(Instant::now())
    }

    pub(crate) fn set_status(&mut self, line: &str) {
        self.status = Some(line.to_string());
        let shown = self.shown_line(line);
        if let Some(tray) = self.tray.as_mut() {
            tray.set_status(&shown);
        }
    }

    pub(crate) fn set_paused(&mut self, paused: bool) {
        self.paused = paused;
        if let Some(tray) = self.tray.as_mut() {
            tray.set_paused(paused);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::rc::Rc;

    #[derive(Default)]
    struct Fake {
        status: Vec<String>,
        paused: Option<bool>,
        /// Answers to `is_shown`, in order; true once used up.
        shown: Vec<bool>,
    }
    struct FakeTray(Rc<RefCell<Fake>>);
    impl TrayUi for FakeTray {
        fn is_shown(&self) -> bool {
            let mut fake = self.0.borrow_mut();
            if fake.shown.is_empty() { true } else { fake.shown.remove(0) }
        }
        fn poll(&self) -> Vec<RecordingTrayCmd> {
            vec![RecordingTrayCmd::Open]
        }
        fn set_status(&mut self, line: &str) {
            self.0.borrow_mut().status.push(line.to_string());
        }
        fn set_paused(&mut self, paused: bool) {
            self.0.borrow_mut().paused = Some(paused);
        }
    }

    /// A builder that fails `fails` times, then succeeds; counts calls.
    fn flaky(fails: u32, calls: Rc<RefCell<u32>>, fake: Rc<RefCell<Fake>>) -> Build<FakeTray> {
        Box::new(move || {
            *calls.borrow_mut() += 1;
            if *calls.borrow() <= fails {
                Err("Shell_NotifyIcon failed".into())
            } else {
                Ok(FakeTray(fake.clone()))
            }
        })
    }

    #[test]
    fn a_tray_that_fails_twice_is_built_on_the_third_try_with_the_state_set_meanwhile() {
        let t0 = Instant::now();
        let calls = Rc::new(RefCell::new(0));
        let fake = Rc::new(RefCell::new(Fake::default()));
        let mut slot = TraySlot::new(flaky(2, calls.clone(), fake.clone()), t0);
        assert!(!slot.is_built());
        slot.set_status("Recall: waiting for a game");
        slot.set_paused(true);
        assert!(slot.poll_at(t0 + Duration::from_secs(1)).is_empty(), "no tray: no commands, no early retry");
        assert_eq!(*calls.borrow(), 1);
        slot.poll_at(t0 + Duration::from_secs(2)); // second failure; next try 4 s later
        assert_eq!(*calls.borrow(), 2);
        slot.poll_at(t0 + Duration::from_secs(5));
        assert_eq!(*calls.borrow(), 2, "backoff doubled to 4 s");
        assert_eq!(slot.poll_at(t0 + Duration::from_secs(6)), vec![RecordingTrayCmd::Open]);
        assert!(slot.is_built());
        assert_eq!(*calls.borrow(), 3);
        assert_eq!(fake.borrow().status, vec!["Recall: waiting for a game".to_string()]);
        assert_eq!(fake.borrow().paused, Some(true));
        slot.poll_at(t0 + Duration::from_secs(600));
        assert_eq!(*calls.borrow(), 3, "a built tray is never rebuilt");
    }

    #[test]
    fn the_retry_never_gives_up_and_caps_at_a_minute() {
        let t0 = Instant::now();
        let calls = Rc::new(RefCell::new(0));
        let mut slot = TraySlot::new(flaky(u32::MAX, calls.clone(), Rc::default()), t0);
        let mut now = t0;
        for _ in 0..40 {
            now += Duration::from_secs(60);
            slot.poll_at(now);
        }
        assert_eq!(*calls.borrow(), 41, "one try per minute at the cap, never stopping");
        assert!(!slot.is_built());
    }

    #[test]
    fn a_tray_the_shell_never_showed_is_rebuilt_until_it_shows() {
        // tray-icon returns Ok when Shell_NotifyIcon(NIM_ADD) times out at sign-in (R2-deep-startup-2).
        let t0 = Instant::now();
        let calls = Rc::new(RefCell::new(0));
        let fake = Rc::new(RefCell::new(Fake { shown: vec![false, false, true], ..Fake::default() }));
        let mut slot = TraySlot::new(flaky(0, calls.clone(), fake.clone()), t0);
        assert_eq!(*calls.borrow(), 1);
        slot.poll_at(t0 + Duration::from_secs(1));
        assert_eq!(*calls.borrow(), 1, "the shell gets a moment before the first check");
        slot.poll_at(t0 + Duration::from_secs(2));
        assert_eq!(*calls.borrow(), 2, "not shown: dropped and built again");
        slot.poll_at(t0 + Duration::from_secs(6));
        assert_eq!(*calls.borrow(), 3, "still not shown: built a third time");
        assert_eq!(slot.poll_at(t0 + Duration::from_secs(14)), vec![RecordingTrayCmd::Open]);
        slot.poll_at(t0 + Duration::from_secs(600));
        assert_eq!(*calls.borrow(), 3, "shown: kept from then on");
        assert!(slot.is_built());
    }

    #[test]
    fn a_tray_that_never_shows_is_kept_after_three_rebuilds() {
        // An icon the shell reports no rect for (whatever the reason) is not rebuilt forever.
        let t0 = Instant::now();
        let calls = Rc::new(RefCell::new(0));
        let fake = Rc::new(RefCell::new(Fake { shown: vec![false; 100], ..Fake::default() }));
        let mut slot = TraySlot::new(flaky(0, calls.clone(), fake.clone()), t0);
        let mut now = t0;
        for _ in 0..30 {
            now += Duration::from_secs(60);
            slot.poll_at(now);
        }
        assert_eq!(*calls.borrow(), 4, "the first build plus three rebuilds");
        assert!(slot.is_built());
    }

    static LINK_DOWN: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    fn link_down_for_test() -> bool {
        LINK_DOWN.load(std::sync::atomic::Ordering::SeqCst)
    }

    #[test]
    fn a_dead_control_pipe_shows_in_the_tray_status() {
        // The pipe gave up (R2-deep-startup-3): the tray says why the app can't reach the recorder.
        let t0 = Instant::now();
        let fake = Rc::new(RefCell::new(Fake::default()));
        let mut slot = TraySlot::new(flaky(0, Rc::default(), fake.clone()), t0).with_link_check(link_down_for_test);
        slot.set_status("Recording VALORANT");
        slot.poll_at(t0 + Duration::from_millis(250));
        assert_eq!(fake.borrow().status.last().map(String::as_str), Some("Recording VALORANT"));
        LINK_DOWN.store(true, std::sync::atomic::Ordering::SeqCst);
        slot.poll_at(t0 + Duration::from_millis(500));
        assert_eq!(fake.borrow().status.last().map(String::as_str), Some("Recording VALORANT · control link unavailable (see log)"));
        slot.set_status("Waiting for a game");
        assert_eq!(fake.borrow().status.last().map(String::as_str), Some("Waiting for a game · control link unavailable (see log)"));
    }
}
