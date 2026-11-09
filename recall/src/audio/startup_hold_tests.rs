//! Merge-gate F4: while the startup hold is set (stdin not drained in time), no device stream
//! opens, input or output, so the first open reads the device the last applied `set_source`
//! names. Fake device and clock (no WASAPI).

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::Duration;

use super::tests::{ring, FakeClock, FakeNotices};
use super::*;

/// A stream that captures one packet and ends.
struct Once;

impl Capture for Once {
    fn endpoint(&self) -> String {
        "Line (Box)".into()
    }

    fn run(&mut self, _: &TrackRing, _: &mut Leveller, _: &dyn Clock, _: &dyn Notices) -> Ended {
        Ended { why: "test".into(), at_once: false }
    }
}

#[test]
fn a_held_track_opens_no_stream_and_starts_as_held() {
    let ring = ring();
    ring.hold_startup();
    ring.live.set_saved_id("{old}");
    let now = Rc::new(Cell::new(Duration::ZERO));
    let mut clock = FakeClock { now: now.clone(), stop_at: Duration::from_secs(3) };
    let (tx, rx) = std::sync::mpsc::channel();
    let opened = RefCell::new(Vec::new());
    let mut open = |_| {
        opened.borrow_mut().push(ring.live.saved_id());
        Ok(Once)
    };
    capture_loop(&ring, "Line (Box)", false, &mut open, tx, &mut clock, &FakeNotices::none(&now));
    assert!(opened.borrow().is_empty(), "no device opens while held");
    assert_eq!(rx.try_recv(), Ok(Ok(())), "the track starts (held), not an init timeout");
    assert_eq!(ring.endpoint(), STARTUP_HELD);
    assert!(ring.snapshot(3.0).iter().all(|s| *s == 0.0), "level with silence");

    // The queued set_source re-picks the device, then the hold ends: the first open reads it.
    ring.live.set_saved_id("{new}");
    ring.release_startup_hold();
    let (tx, _rx) = std::sync::mpsc::channel();
    let mut clock = FakeClock { now: now.clone(), stop_at: now.get() + Duration::from_secs(1) };
    capture_loop(&ring, "Line (Box)", false, &mut open, tx, &mut clock, &FakeNotices::none(&now));
    assert_eq!(opened.borrow().first().map(String::as_str), Some("{new}"), "the device the user picked last");
}
