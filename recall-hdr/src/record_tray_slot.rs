//! The recording tray, retried until it exists (audit G8). At sign-in the Run-key start can
//! reach `Shell_NotifyIcon` before Explorer's notification area is ready; a failed build used
//! to end the controller (exit 2), leaving nothing recording all session. Now the controller
//! records without a tray and the pump retries the build with backoff (2 s doubling to 60 s,
//! never giving up). Status and pause changes made meanwhile are applied once it exists.

use std::time::{Duration, Instant};

use crate::log;
use crate::record_tray::{RecordingTray, RecordingTrayCmd};

const FIRST_RETRY: Duration = Duration::from_secs(2);
const MAX_RETRY: Duration = Duration::from_secs(60);

/// What the slot needs from a tray (the real one, or a test double).
pub(crate) trait TrayUi {
    fn poll(&self) -> Vec<RecordingTrayCmd>;
    fn set_status(&mut self, line: &str);
    fn set_paused(&mut self, paused: bool);
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
}

impl TraySlot<RecordingTray> {
    /// The real tray, first tried now.
    pub(crate) fn recording(now: Instant) -> Self {
        Self::new(Box::new(RecordingTray::build), now)
    }
}

impl<T: TrayUi> TraySlot<T> {
    /// Tries `build` once at `now`; later tries happen in [`TraySlot::poll_at`].
    pub(crate) fn new(build: Build<T>, now: Instant) -> Self {
        let mut slot = TraySlot { tray: None, build, next_try: now, delay: FIRST_RETRY, failures: 0, status: None, paused: false };
        slot.try_build(now);
        slot
    }

    pub(crate) fn is_built(&self) -> bool {
        self.tray.is_some()
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
                    tray.set_status(line);
                }
                tray.set_paused(self.paused);
                self.tray = Some(tray);
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

    /// Retries a missing tray when due, then drains its commands.
    pub(crate) fn poll_at(&mut self, now: Instant) -> Vec<RecordingTrayCmd> {
        self.try_build(now);
        self.tray.as_ref().map(TrayUi::poll).unwrap_or_default()
    }

    pub(crate) fn poll(&mut self) -> Vec<RecordingTrayCmd> {
        self.poll_at(Instant::now())
    }

    pub(crate) fn set_status(&mut self, line: &str) {
        self.status = Some(line.to_string());
        if let Some(tray) = self.tray.as_mut() {
            tray.set_status(line);
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
    }
    struct FakeTray(Rc<RefCell<Fake>>);
    impl TrayUi for FakeTray {
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
}
