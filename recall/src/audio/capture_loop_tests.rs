//! The reopen loop with a fake device and a fake clock (no WASAPI, no real endpoints).

use super::*;
use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, AtomicU32};
use std::sync::Mutex;

const RATE: u32 = 100; // one frame per 10 ms, mono

pub(super) fn ring() -> TrackRing {
    TrackRing {
        name: "in-1",
        title: Mutex::new("Voice"),
        source: "mic",
        gain: AtomicU32::new(1.0f32.to_bits()),
        boosted: AtomicBool::new(false),
        input: true,
        mic_mute: AtomicBool::new(true),
        on_default: true,
        opened_own: AtomicBool::new(false),
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
pub(super) struct FakeClock {
    pub(super) now: Rc<Cell<Duration>>,
    pub(super) stop_at: Duration,
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

/// A device that plays `secs` of 0.5 in 20 ms packets, then ends as `at_once` says. `trace`
/// gets (clock, ring length) after every packet.
struct FakeStream {
    now: Rc<Cell<Duration>>,
    secs: u64,
    at_once: bool,
    endpoint: String,
    trace: Rc<RefCell<Vec<(Duration, usize)>>>,
}

fn fake(now: &Rc<Cell<Duration>>, secs: u64, at_once: bool) -> FakeStream {
    FakeStream { now: now.clone(), secs, at_once, endpoint: "Headset (USB)".into(), trace: Rc::default() }
}

impl Capture for FakeStream {
    fn endpoint(&self) -> String {
        self.endpoint.clone()
    }

    fn run(&mut self, ring: &TrackRing, level: &mut Leveller, _clock: &dyn Clock, _notices: &dyn Notices) -> Ended {
        for _ in 0..self.secs * 50 {
            self.now.set(self.now.get() + Duration::from_millis(20));
            ring.push_capture(&level.live(self.now.get(), &[0.5; 2], 1), false);
            self.trace.borrow_mut().push((self.now.get(), len(ring)));
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
            Ok(fake(&now, 1, false))
        }
    };
    capture_loop(&ring, " Headset (USB) ", true, open, tx, &mut clock, &FakeNotices::none(&now));

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
    capture_loop(&ring, "Test Mic", false, open, tx, &mut clock, &FakeNotices::none(&now));
    assert_eq!(rx.try_recv().unwrap().unwrap_err(), "no endpoint matches \"Test Mic\" (RECALL_AUDIO_EXACT)");
    assert_eq!((opens.get(), len(&ring), now.get()), (1, 0, Duration::ZERO), "no track, no retries");
}

#[test]
fn a_device_source_outside_tests_waits_for_its_device() {
    assert!(waits_when_absent(false, false));
    assert!(!waits_when_absent(true, false), "a process-loopback target is reported, not waited on");
    assert!(!waits_when_absent(false, true), "RECALL_AUDIO_EXACT keeps no track for a missing endpoint");
    assert_eq!(waiting_label(" Headset (USB) "), "Headset (USB) (not connected; waiting)");
}

/// 2026-11-07 audit A1: a source on the Windows default (the fresh-install Microphone) with no
/// default device at engine start (headset off, endpoint held in exclusive mode) still gets its
/// track: silent and waiting, and it records once a default appears.
#[test]
fn a_default_source_with_nothing_to_open_at_start_waits_for_a_default() {
    assert_eq!(waiting_label("  "), "Windows default (none yet; waiting)");

    let ring = ring();
    let now = Rc::new(Cell::new(Duration::ZERO));
    let mut clock = FakeClock { now: now.clone(), stop_at: Duration::from_secs(5) };
    let (tx, rx) = std::sync::mpsc::channel();
    let opened = Cell::new(false);
    let open = |_reopen: bool| {
        // No default until 2 s; the default opens once, then is gone again.
        if now.get() < Duration::from_secs(2) || opened.replace(true) {
            return Err("default endpoint failed: element not found".to_owned());
        }
        let mut stream = fake(&now, 1, false);
        stream.endpoint = "Headset (USB) (Windows default)".into();
        Ok(stream)
    };
    capture_loop(&ring, "", waits_when_absent(false, false), open, tx, &mut clock, &FakeNotices::none(&now));
    assert_eq!(rx.try_recv().unwrap(), Ok(()), "the track starts while there is no default");
    let samples = ring.snapshot(5.0);
    assert_eq!(len(&ring), 500, "level with the clock");
    assert!(samples[..200].iter().all(|&s| s == 0.0), "silence while there is no default");
    assert!(samples[200..300].iter().all(|&s| s == 0.5), "the new default's sound from 2 s");
    assert!(samples[300..].iter().all(|&s| s == 0.0), "silence once it is gone again");
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
        Ok::<_, String>(fake(&now, 1, true))
    };
    capture_loop(&ring, "", false, open, tx, &mut clock, &FakeNotices::none(&now));
    let at = |s| Duration::from_secs(s);
    assert_eq!(opens.into_inner()[..3], [(false, at(0)), (true, at(1)), (true, at(2))]);
    assert!(ring.snapshot(3.0).iter().all(|&s| s == 0.5), "no silent gap between the two devices");
}

/// Notices as Windows would send them: devices changed once the clock passes `devices_at`; a
/// default id the test sets.
pub(super) struct FakeNotices {
    now: Rc<Cell<Duration>>,
    devices_at: Cell<Option<Duration>>,
    default: RefCell<Option<String>>,
}

impl FakeNotices {
    pub(super) fn none(now: &Rc<Cell<Duration>>) -> Self {
        FakeNotices { now: now.clone(), devices_at: Cell::new(None), default: RefCell::new(None) }
    }
}

impl Notices for FakeNotices {
    fn take_devices_changed(&self) -> bool {
        let fired = self.devices_at.get().is_some_and(|at| self.now.get() >= at);
        if fired {
            self.devices_at.set(None); // taken: acted on once
        }
        fired
    }

    fn take_default(&self) -> Option<String> {
        self.default.borrow_mut().take()
    }
}

/// A lost device is retried at 2 s, then backs off to 4 s, 8 s and at most 10 s.
#[test]
fn reopen_backs_off_to_ten_seconds() {
    let s = |n| reopen_wait(n).as_secs();
    assert_eq!((s(0), s(1), s(2), s(3), s(4), s(50)), (2, 2, 4, 8, 10, 10));
}

/// A named source never opens another device: with the Windows default connected and the
/// named device not, the track starts silent and waiting, and opens by name once it appears.
#[test]
fn a_named_source_never_opens_the_default_and_opens_by_name_when_it_appears() {
    use crate::audio::{choose_endpoint, Pick};
    let ring = ring();
    let now = Rc::new(Cell::new(Duration::ZERO));
    let mut clock = FakeClock { now: now.clone(), stop_at: Duration::from_secs(8) };
    let (tx, rx) = std::sync::mpsc::channel();
    let attempts = RefCell::new(Vec::new());
    let open = |reopen: bool| {
        // The fake enumerator: the Windows default mic is always there; the headset from 3 s.
        let mut names = vec!["Mic (Realtek)".to_owned()];
        if now.get() >= Duration::from_secs(3) {
            names.push("Headset (USB)".to_owned());
        }
        attempts.borrow_mut().push((reopen, now.get(), ring.endpoint()));
        let mut stream = fake(&now, 1, false);
        stream.endpoint = match choose_endpoint(&names, Some("Headset (USB)"), false)? {
            Pick::Named(i) => names[i].clone(),
            Pick::Default => "Mic (Realtek) (Windows default)".to_owned(),
        };
        Ok::<_, String>(stream)
    };
    capture_loop(&ring, "Headset (USB)", true, open, tx, &mut clock, &FakeNotices::none(&now));

    assert_eq!(rx.try_recv().unwrap(), Ok(()), "the track starts");
    let attempts = attempts.into_inner();
    let times: Vec<(bool, Duration)> = attempts.iter().map(|a| (a.0, a.1)).collect();
    let at = Duration::from_secs;
    assert_eq!(times, [(false, at(0)), (true, at(2)), (true, at(6))], "no open until the headset is there: {attempts:?}");
    assert_eq!(attempts[1].2, "Headset (USB) (not connected; waiting)");
    let samples = ring.snapshot(8.0);
    assert_eq!(len(&ring), 800, "level with the clock");
    assert!(samples[..600].iter().all(|&s| s == 0.0), "silent while waiting, though a default mic is connected");
    assert!(samples[600..700].iter().all(|&s| s == 0.5), "the headset, by name, from 6 s");
}

#[test]
fn choose_endpoint_never_falls_back_for_a_named_source() {
    use crate::audio::{choose_endpoint, Pick};
    let names = ["Mic (Realtek)".to_owned(), "Headset (USB)".to_owned()];
    assert_eq!(choose_endpoint(&names, Some("Headset (USB)"), false), Ok(Pick::Named(1)));
    assert_eq!(choose_endpoint(&names, Some("headset usb"), false), Err("'headset usb' is not connected".to_owned()), "no word matching");
    assert_eq!(choose_endpoint(&names[..1], Some("Headset (USB)"), false), Err("'Headset (USB)' is not connected".to_owned()));
    assert_eq!(choose_endpoint(&names, None, false), Ok(Pick::Default), "only a source with no device follows the default");
    assert_eq!(choose_endpoint(&names, Some("  "), false), Ok(Pick::Default));
    assert!(choose_endpoint(&names, None, true).unwrap_err().contains("RECALL_AUDIO_EXACT"));
    assert!(choose_endpoint(&names[..1], Some("Headset (USB)"), true).unwrap_err().contains("RECALL_AUDIO_EXACT"));
}

/// The Windows default moved: the new device takes 120 ms to open. Right after the switch the
/// track is level with the clock again, within one frame (10 ms at this test's 100 Hz), so it
/// stays in sync with the video instead of lagging by the switch time.
#[test]
fn a_switch_at_once_keeps_the_track_level_with_the_clock() {
    let ring = ring();
    let now = Rc::new(Cell::new(Duration::ZERO));
    let mut clock = FakeClock { now: now.clone(), stop_at: Duration::from_millis(2_120) };
    let (tx, _rx) = std::sync::mpsc::channel();
    let trace = Rc::new(RefCell::new(Vec::new()));
    let open = |reopen: bool| {
        if reopen {
            now.set(now.get() + Duration::from_millis(120)); // opening the new device
        }
        let mut stream = fake(&now, 1, !reopen);
        if reopen {
            stream.trace = trace.clone();
        }
        Ok::<_, String>(stream)
    };
    capture_loop(&ring, "", false, open, tx, &mut clock, &FakeNotices::none(&now));

    let trace = trace.borrow();
    assert_eq!(trace.len(), 50, "the second device played");
    for &(t, samples) in trace.iter() {
        let due = (t.as_millis() / 10) as usize;
        assert!(samples.abs_diff(due) <= 1, "at {t:?}: {samples} frames, clock {due}");
    }
}

/// A waiting track does not sit out its backoff when Windows says a device arrived: it tries
/// its device by name at once.
#[test]
fn a_waiting_track_tries_its_device_at_once_when_one_arrives() {
    let ring = ring();
    let now = Rc::new(Cell::new(Duration::ZERO));
    let mut clock = FakeClock { now: now.clone(), stop_at: Duration::from_secs(5) };
    let (tx, _rx) = std::sync::mpsc::channel();
    let notices = FakeNotices::none(&now);
    notices.devices_at.set(Some(Duration::from_millis(3_000))); // plugged in at 3 s
    let opens = RefCell::new(Vec::new());
    let open = |reopen: bool| {
        opens.borrow_mut().push((reopen, now.get()));
        if now.get() < Duration::from_secs(3) {
            return Err("'Headset (USB)' is not connected".to_owned());
        }
        Ok(fake(&now, 1, false))
    };
    capture_loop(&ring, "Headset (USB)", true, open, tx, &mut clock, &notices);
    let at = Duration::from_millis;
    // Backoff alone would try at 6 s (2 s, then 4 s).
    assert_eq!(opens.into_inner()[..3], [(false, at(0)), (true, at(2_000)), (true, at(3_000))]);
    let samples = ring.snapshot(5.0);
    assert_eq!(len(&ring), 500, "level with the clock");
    assert!(samples[300..400].iter().all(|&s| s == 0.5), "the headset from 3 s");
}

#[test]
fn only_a_default_source_follows_a_moved_default() {
    let now = Rc::new(Cell::new(Duration::ZERO));
    let notices = FakeNotices::none(&now);
    let mut watch = watch_for(" ", Some("{dev-a}".into())).expect("a source with no device is watched");
    assert!(watch.check(&notices).is_none(), "no notice");
    *notices.default.borrow_mut() = Some("{dev-a}".into());
    assert!(watch.check(&notices).is_none(), "the same device");
    *notices.default.borrow_mut() = Some("{dev-b}".into());
    let moved = watch.check(&notices).expect("the default moved");
    assert!(moved.at_once);
    assert!(watch.check(&notices).is_none(), "the notice was taken");
    assert!(watch_for("Headset (USB)", Some("{dev-a}".into())).is_none(), "a named source never follows the default");
    assert!(watch_for("", None).is_none(), "no default id, nothing to compare");
}

/// A device whose stream wakes from a 3 h sleep: the first packet after it, and what each
/// `level.live` returned.
struct SleepyStream {
    now: Rc<Cell<Duration>>,
    out: Rc<RefCell<Vec<usize>>>,
}

impl Capture for SleepyStream {
    fn endpoint(&self) -> String {
        "Headset (USB)".into()
    }

    fn run(&mut self, ring: &TrackRing, level: &mut Leveller, _clock: &dyn Clock, _notices: &dyn Notices) -> Ended {
        self.now.set(self.now.get() + Duration::from_secs(3 * 3600)); // the PC slept
        let out = level.live(self.now.get(), &[0.5; 2], 1);
        self.out.borrow_mut().push(out.len());
        ring.push_capture(&out, false);
        Ended { why: "device lost".into(), at_once: false }
    }
}

/// 2026-11-07 audit T1: the capture loop's leveller owes at most one ring after a sleep (the
/// stream's lead and the silence after it ends), however long the sleep was.
#[test]
fn a_sleep_owes_at_most_one_ring_through_the_capture_loop() {
    let ring = ring();
    let cap = ring.capacity_frames() as usize;
    let now = Rc::new(Cell::new(Duration::ZERO));
    let mut clock = FakeClock { now: now.clone(), stop_at: Duration::from_secs(3 * 3600 + 1) };
    let (tx, _rx) = std::sync::mpsc::channel();
    let out = Rc::new(RefCell::new(Vec::new()));
    let open = |_reopen: bool| Ok::<_, String>(SleepyStream { now: now.clone(), out: out.clone() });
    capture_loop(&ring, "", false, open, tx, &mut clock, &FakeNotices::none(&now));

    let out = out.borrow();
    assert_eq!(out.len(), 1, "{out:?}");
    assert!(out[0] <= cap + 2, "the first packet after the sleep carried {} frames (ring {cap})", out[0]);
    assert_eq!(len(&ring), cap, "the ring is full and no bigger");
}

/// A device notice that arrived while the stream was live (another device plugged in) is
/// stale by the time that stream is lost: the first reopen still waits its 2 s.
#[test]
fn a_notice_during_a_live_stream_does_not_cut_the_next_backoff() {
    let ring = ring();
    let now = Rc::new(Cell::new(Duration::ZERO));
    let mut clock = FakeClock { now: now.clone(), stop_at: Duration::from_millis(3_500) };
    let (tx, _rx) = std::sync::mpsc::channel();
    let notices = FakeNotices::none(&now);
    notices.devices_at.set(Some(Duration::from_millis(500))); // during the first stream
    let opens = RefCell::new(Vec::new());
    let open = |reopen: bool| {
        opens.borrow_mut().push((reopen, now.get()));
        if reopen {
            return Err("'Headset (USB)' is not connected".to_owned());
        }
        Ok(fake(&now, 1, false))
    };
    capture_loop(&ring, "Headset (USB)", true, open, tx, &mut clock, &notices);
    let at = Duration::from_millis;
    // Lost at 1 s: the reopen is at 3 s (1 s + REOPEN_EVERY), not at once.
    assert_eq!(opens.into_inner(), [(false, at(0)), (true, at(3_000))]);
}

#[test]
fn a_process_loopback_track_registers_no_device_notices() {
    let none = notices_for::<()>(true, || panic!("a loopback track registered device notices"));
    assert!(none.is_none());
    let calls = Cell::new(0);
    let some = notices_for(false, || {
        calls.set(calls.get() + 1);
        Ok::<_, String>(7)
    });
    assert_eq!((some, calls.get()), (Some(Ok(7)), 1), "a device track registers once");
}
