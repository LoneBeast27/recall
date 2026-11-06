//! The reopen loop with a fake device and a fake clock (no WASAPI, no real endpoints).

use super::*;
use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, AtomicU32};
use std::sync::Mutex;

const RATE: u32 = 100; // one frame per 10 ms, mono

fn ring() -> TrackRing {
    TrackRing {
        name: "in-1",
        title: Mutex::new("Voice"),
        source: "mic",
        gain: AtomicU32::new(1.0f32.to_bits()),
        boosted: AtomicBool::new(false),
        input: true,
        endpoint: Mutex::new(String::new()),
        rate: RATE,
        channels: 1,
        samples: Mutex::new(VecDeque::new()),
        capacity: RATE as usize * 120,
    }
}

fn len(ring: &TrackRing) -> usize {
    ring.samples.lock().unwrap().len()
}

/// Moves on by each pause; ends the loop once past `stop_at`.
struct FakeClock {
    now: Rc<Cell<Duration>>,
    stop_at: Duration,
}

impl Clock for FakeClock {
    fn elapsed(&self) -> Duration {
        self.now.get()
    }

    fn pause(&mut self, step: Duration) -> bool {
        self.now.set(self.now.get() + step);
        self.now.get() <= self.stop_at
    }
}

/// A device that plays `secs` of 0.5 in 20 ms packets, then ends as `at_once` says.
struct FakeStream {
    now: Rc<Cell<Duration>>,
    secs: u64,
    at_once: bool,
}

impl Capture for FakeStream {
    fn endpoint(&self) -> String {
        "Headset (USB)".into()
    }

    fn run(&mut self, ring: &TrackRing, level: &mut Leveller, _clock: &dyn Clock) -> Ended {
        for _ in 0..self.secs * 50 {
            self.now.set(self.now.get() + Duration::from_millis(20));
            ring.push_capture(&level.live(self.now.get(), &[0.5; 2], 1), false);
        }
        Ended { why: "device lost".into(), at_once: self.at_once }
    }
}

/// What the loop saw at one open attempt.
#[derive(Debug, PartialEq)]
struct Attempt {
    reopen: bool,
    at: Duration,
    samples: usize,
    endpoint: String,
    track_started: bool,
}

#[test]
fn a_named_device_absent_at_start_keeps_a_level_track_and_opens_when_it_appears() {
    let ring = ring();
    let now = Rc::new(Cell::new(Duration::ZERO));
    let mut clock = FakeClock { now: now.clone(), stop_at: Duration::from_secs(16) };
    let (tx, rx) = std::sync::mpsc::channel();
    let started = Cell::new(false);
    let attempts = RefCell::new(Vec::new());
    let open = |reopen: bool| {
        // The track's start was answered before this attempt (any attempt after the first).
        if let Ok(answer) = rx.try_recv() {
            assert_eq!(answer, Ok(()), "the track starts while its device is absent");
            started.set(true);
        }
        let n = attempts.borrow().len();
        attempts.borrow_mut().push(Attempt { reopen, at: now.get(), samples: len(&ring), endpoint: ring.endpoint(), track_started: started.get() });
        if n < 3 {
            Err("'Headset (USB)' is not connected".to_owned())
        } else {
            Ok(FakeStream { now: now.clone(), secs: 1, at_once: false })
        }
    };
    capture_loop(&ring, " Headset (USB) ", true, open, tx, &mut clock);

    let waiting = "Headset (USB) (not connected; waiting)";
    let seen = attempts.into_inner();
    let at = |s| Duration::from_secs(s);
    // Opens at 0, then backing off 2 s, 4 s, 8 s; every reopen by the device's own name.
    let want = [(false, at(0), false), (true, at(2), true), (true, at(6), true), (true, at(14), true)];
    assert_eq!(seen.len(), want.len(), "{seen:?}");
    for (a, (reopen, t, started)) in seen.iter().zip(want) {
        assert_eq!((a.reopen, a.at, a.track_started), (reopen, t, started), "{a:?}");
        // Level with the clock while disconnected: one silent frame per 10 ms so far.
        assert_eq!(a.samples, (t.as_millis() / 10) as usize, "{a:?}");
    }
    assert!(seen[1..].iter().all(|a| a.endpoint == waiting), "status while waiting: {seen:?}");

    let samples = ring.snapshot(16.0);
    assert_eq!(len(&ring), 1600, "still level after the device came and went");
    assert!(samples[..1400].iter().all(|&s| s == 0.0), "silence while not connected");
    assert!(samples[1400..1500].iter().all(|&s| s == 0.5), "the device's sound from 14 s, on the clock");
    assert!(samples[1500..].iter().all(|&s| s == 0.0), "silence after it was lost");
    assert_eq!(ring.endpoint(), "Headset (USB) (lost: device lost; reopening)");
}

#[test]
fn without_waiting_a_failed_first_open_ends_the_track_as_before() {
    let ring = ring();
    let now = Rc::new(Cell::new(Duration::ZERO));
    let mut clock = FakeClock { now: now.clone(), stop_at: Duration::from_secs(30) };
    let (tx, rx) = std::sync::mpsc::channel();
    let opens = Cell::new(0);
    let open = |_reopen: bool| -> Result<FakeStream, String> {
        opens.set(opens.get() + 1);
        Err("no endpoint matches \"Test Mic\" (RECALL_AUDIO_EXACT)".to_owned())
    };
    capture_loop(&ring, "Test Mic", false, open, tx, &mut clock);
    assert_eq!(rx.try_recv().unwrap().unwrap_err(), "no endpoint matches \"Test Mic\" (RECALL_AUDIO_EXACT)");
    assert_eq!((opens.get(), len(&ring), now.get()), (1, 0, Duration::ZERO), "no track, no retries");
}

#[test]
fn only_a_named_device_outside_tests_waits_for_its_device() {
    assert!(waits_when_absent("Headset (USB)", false, false));
    assert!(!waits_when_absent("  ", false, false), "the Windows default has no name to wait for");
    assert!(!waits_when_absent("Headset (USB)", true, false), "a process-loopback target is reported, not waited on");
    assert!(!waits_when_absent("Headset (USB)", false, true), "RECALL_AUDIO_EXACT keeps no track for a missing endpoint");
    assert_eq!(waiting_label(" Headset (USB) "), "Headset (USB) (not connected; waiting)");
}

#[test]
fn a_moved_windows_default_reopens_at_once_without_a_gap() {
    let ring = ring();
    let now = Rc::new(Cell::new(Duration::ZERO));
    let mut clock = FakeClock { now: now.clone(), stop_at: Duration::from_secs(3) };
    let (tx, _rx) = std::sync::mpsc::channel();
    let opens = RefCell::new(Vec::new());
    let open = |reopen: bool| {
        opens.borrow_mut().push((reopen, now.get()));
        Ok::<_, String>(FakeStream { now: now.clone(), secs: 1, at_once: true })
    };
    capture_loop(&ring, "", false, open, tx, &mut clock);
    let at = |s| Duration::from_secs(s);
    assert_eq!(opens.into_inner()[..3], [(false, at(0)), (true, at(1)), (true, at(2))]);
    assert!(ring.snapshot(3.0).iter().all(|&s| s == 0.5), "no silent gap between the two devices");
}

#[test]
fn the_default_watch_checks_every_two_seconds_and_fires_on_another_id() {
    use crate::audio::stream::DefaultWatch;
    let at = Duration::from_millis;
    let mut watch = DefaultWatch::new("{dev-a}".into(), at(0));
    assert!(!watch.moved(at(1_900), || panic!("checked before 2 s")));
    assert!(!watch.moved(at(2_000), || Some("{dev-a}".into())), "same device");
    assert!(!watch.moved(at(4_100), || None), "no default now: the stream reports the loss itself");
    assert!(!watch.moved(at(5_000), || panic!("checked again within 2 s")));
    assert!(watch.moved(at(6_200), || Some("{dev-b}".into())), "the default moved");
}

/// A lost device is retried at 2 s, then backs off to 4 s, 8 s and at most 10 s.
#[test]
fn reopen_backs_off_to_ten_seconds() {
    let s = |n| reopen_wait(n).as_secs();
    assert_eq!((s(0), s(1), s(2), s(3), s(4), s(50)), (2, 2, 4, 8, 10, 10));
}
