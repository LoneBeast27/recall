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

/// design-audio-mute P4: a live track retired (removed, disabled or re-picked under another
/// name): its next packet is silence, its stream ends at the next packet turn, and no stream
/// opens again, the sync release included.
#[test]
fn a_retire_silences_the_next_packet_and_closes_the_stream() {
    let ring = ring();
    ring.push_live(&[0.5; 960], super::super::MuteNow { muted: false, presses: 0 });
    assert!(ring.snapshot(0.01).iter().any(|s| *s != 0.0), "live before the retire");
    ring.retire();
    ring.push_live(&[0.5; 960], super::super::MuteNow { muted: false, presses: 0 });
    assert!(ring.snapshot(0.01).iter().all(|s| *s == 0.0), "the next packet is silence");
    assert!(ring.live.take_repick(), "the open stream ends at its next packet turn");
    ring.release_startup_hold(); // a sync after it
    assert!(ring.startup_held(), "a retire is never released");
    let now = Rc::new(Cell::new(Duration::ZERO));
    let mut clock = FakeClock { now: now.clone(), stop_at: Duration::from_secs(3) };
    let (tx, _rx) = std::sync::mpsc::channel();
    let opened = Cell::new(0);
    let mut open = |_| {
        opened.set(opened.get() + 1);
        Ok(Once)
    };
    capture_loop(&ring, "Line (Box)", false, &mut open, tx, &mut clock, &FakeNotices::none(&now));
    assert_eq!(opened.get(), 0, "no stream opens again");
    assert!(ring.retired() && ring.held_now());
}

/// P18: a capture thread that does not answer its init in time is retired (silence, no stream).
#[test]
fn an_init_timeout_retires_the_orphan_ring() {
    let ring = ring();
    let (_tx, rx) = std::sync::mpsc::channel();
    assert_eq!(super::super::hold::await_init(&ring, &rx, Duration::from_millis(10)), Err("audio init timed out".to_owned()));
    assert!(ring.retired() && ring.startup_held(), "the orphan ring stores silence and opens nothing");
    let (tx, rx) = std::sync::mpsc::channel();
    tx.send(Ok(())).unwrap();
    let fresh = super::tests::ring();
    assert_eq!(super::super::hold::await_init(&fresh, &rx, Duration::from_millis(10)), Ok(()));
    assert!(!fresh.retired());
    let audio = include_str!("../audio.rs").split_whitespace().collect::<Vec<_>>().join(" ");
    assert!(audio.contains("hold::await_init(&ring, &init_rx, std::time::Duration::from_secs(5)).map(|()| ring)"), "start_track waits through it");
}
