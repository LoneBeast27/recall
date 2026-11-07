//! R7-deep-audio-1/3/4: a named stream re-checks its pick on device notices. Fake streams,
//! clock and notices; probes return made-up keys (no WASAPI, no real endpoints).

use super::*;
use crate::audio::apps::Leveller;
use crate::audio::capture_loop::tests::{ring, FakeClock, FakeNotices};
use crate::audio::capture_loop::{capture_loop, Capture, Clock};
use crate::audio::TrackRing;
use std::cell::Cell;
use std::rc::Rc;
use std::time::{Duration, Instant};

fn id(s: &str) -> Key {
    Key::Id(s.to_owned())
}

fn name(s: &str) -> Key {
    Key::Name(s.to_owned())
}

fn probe(answer: Option<Key>) -> Probe {
    Arc::new(move || answer.clone())
}

/// Plays 0.5 in 20 ms packets, not muted (a line-in off Alt+M, or an output), checking its
/// rebind before every packet.
struct Playing {
    now: Rc<Cell<Duration>>,
    endpoint: &'static str,
    rebind: Option<Rebind>,
}

impl Capture for Playing {
    fn endpoint(&self) -> String {
        self.endpoint.to_owned()
    }

    fn run(&mut self, ring: &TrackRing, level: &mut Leveller, _clock: &dyn Clock, notices: &dyn Notices) -> Ended {
        for _ in 0..100 {
            if let Some(ended) = self.rebind.as_mut().and_then(|r| r.check(notices)) {
                return ended;
            }
            self.now.set(self.now.get() + Duration::from_millis(20));
            ring.push_capture(&level.live(self.now.get(), &[0.5; 2], 1), false);
        }
        Ended { why: "device lost".into(), at_once: false }
    }
}

/// One track: the first open is `first` (key `opened`) with a rebind whose re-pick answers
/// `repick`; a notice comes at `notice`; every later open gives `later` (None = no unique
/// endpoint: not connected). `open_takes` moves the clock during each open. Returns the number
/// of opens and the samples held.
fn track(opened: Key, repick: Option<Key>, later: Option<&'static str>, notice: Duration, open_takes: Duration) -> (u32, Vec<f32>) {
    let ring = TrackRing { on_default: false, ..ring() };
    let now = Rc::new(Cell::new(Duration::ZERO));
    let mut clock = FakeClock { now: now.clone(), stop_at: Duration::from_millis(1_500) };
    let notices = FakeNotices::devices_at(&now, notice);
    let opens = Cell::new(0u32);
    let (tx, _rx) = std::sync::mpsc::channel();
    let (opened, repick) = (Cell::new(Some(opened)), Cell::new(Some(repick)));
    let open = |_reopen: bool| {
        opens.set(opens.get() + 1);
        now.set(now.get() + open_takes);
        if opens.get() == 1 {
            let rebind = rebind_for("Mic (Box)", opened.take().unwrap(), probe(repick.take().unwrap()), (Run::Inline, Default::default()));
            return Ok(Playing { now: now.clone(), endpoint: "2- Mic (Box)", rebind });
        }
        later.map(|endpoint| Playing { now: now.clone(), endpoint, rebind: None }).ok_or_else(|| "no unique endpoint".to_owned())
    };
    capture_loop(&ring, "Mic (Box)", true, open, tx, &mut clock, &notices);
    let held = ring.samples.lock().unwrap().iter().copied().collect();
    (opens.get(), held)
}

/// R7-deep-audio-1: on the fallback "2- Mic (Box)", its own device returns renumbered next to it
/// ("3- Mic (Box)"): two stripped matches, so the re-pick opens none (A4). The stream ends and
/// the track waits in silence; before, it kept recording the sibling for the session.
#[test]
fn an_ambiguous_repick_ends_the_fallback_stream() {
    let (opens, held) = track(id("{two}"), None, None, Duration::from_millis(500), Duration::ZERO);
    assert!(opens >= 2, "the fallback ended and the track tried to reopen: {opens} opens");
    assert!(held[..40].contains(&0.5), "the fallback recorded until the notice");
    assert!(!held[60..].contains(&0.5), "never the sibling once the pick changed");
}

/// R7-deep-audio-1: a source with no saved id (a legacy line-in, any named output) on the
/// fallback goes back to the exact name when it returns; before, it never could.
#[test]
fn a_no_id_stream_goes_back_to_its_exact_name() {
    let (opens, held) = track(name("2- Mic (Box)"), Some(name("Mic (Box)")), Some("Mic (Box)"), Duration::from_millis(500), Duration::ZERO);
    assert_eq!(opens, 2, "the fallback ended and \"Mic (Box)\" opened");
    assert!(held[60..].contains(&0.5), "recording again on its own name");
}

/// The same pick: the stream stays (no reopen storm on every notice).
#[test]
fn an_unchanged_repick_keeps_the_stream() {
    let (opens, _) = track(id("{two}"), Some(id("{two}")), Some("Mic (Box)"), Duration::from_millis(500), Duration::ZERO);
    assert_eq!(opens, 1, "the stream played on to its own end; the clock stopped before a reopen");
}

/// R7-deep-audio-3: a notice that came while the stream was opening (the own device returned
/// meanwhile) is queued for the stream's first check, not drained as stale.
#[test]
fn a_notice_during_the_open_is_not_dropped() {
    let (opens, held) = track(id("{two}"), Some(id("{own}")), Some("Mic (Box)"), Duration::from_millis(10), Duration::from_millis(30));
    assert_eq!(opens, 2, "the fallback ended at its first check and its own device opened");
    assert!(held[10..].contains(&0.5));
}

/// R7-deep-audio-4: a source with no saved id never reads endpoint ids (the probe compares
/// names); one with an id reads it.
#[test]
fn a_source_without_a_saved_id_never_reads_endpoint_ids() {
    assert_eq!(key_for("", "2- Mic (Box)", || panic!("read an endpoint id")), name("2- Mic (Box)"));
    assert_eq!(key_for("x\u{7}", "Mic (Box)", || panic!("read an endpoint id")), name("Mic (Box)"), "a malformed id is none");
    assert_eq!(key_for("{a}", "Mic (Box)", || Some(" {a} ".into())), id("{a}"));
    assert_eq!(key_for("{a}", "Mic (Box)", || None), name("Mic (Box)"), "no readable id: the name, on both sides");
}

/// R7-deep-audio-4: the re-pick runs off the capture loop: a slow enumeration (a USB arrival
/// with a busy audiosrv) never holds a check, and its answer ends the stream when it lands.
#[test]
fn a_slow_repick_never_holds_the_capture_loop() {
    let now = Rc::new(Cell::new(Duration::ZERO));
    let notices = FakeNotices::devices_at(&now, Duration::ZERO);
    let slow: Probe = Arc::new(|| {
        std::thread::sleep(Duration::from_millis(300));
        None
    });
    let mut rebind = rebind_for("Mic (Box)", id("{two}"), slow, (Run::Thread, Default::default())).expect("a named stream rebinds");
    let start = Instant::now();
    let first = rebind.check(&notices);
    assert!(start.elapsed() < Duration::from_millis(100), "the check waited {:?} for the enumeration", start.elapsed());
    assert!(first.is_none(), "the answer is not in yet");
    let mut ended = None;
    while ended.is_none() && start.elapsed() < Duration::from_secs(5) {
        std::thread::sleep(Duration::from_millis(10));
        ended = rebind.check(&notices);
    }
    assert!(ended.is_some_and(|e| e.at_once), "the ambiguous answer ended the stream");
}

#[test]
fn only_a_named_device_rebinds() {
    assert!(rebind_for(" ", name("Speakers (Box)"), probe(None), (Run::Inline, Default::default())).is_none(), "the Windows default is watched instead");
    assert!(rebind_for("Speakers (Box)", name("Speakers (Box)"), probe(None), (Run::Inline, Default::default())).is_some(), "a named output too");
}

/// p9 gate item 3: at most one re-pick thread per track. A new stream's rebind while the last
/// stream's re-pick still runs skips (it keeps its notice) instead of spawning another.
#[test]
fn at_most_one_repick_thread_runs_per_track() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    let started = Arc::new(AtomicUsize::new(0));
    let (release, gate) = std::sync::mpsc::channel::<()>();
    let gate = Arc::new(std::sync::Mutex::new(gate));
    let counted = started.clone();
    let blocked: Probe = Arc::new(move || {
        counted.fetch_add(1, Ordering::SeqCst);
        let _ = gate.lock().unwrap().recv_timeout(Duration::from_secs(5));
        None
    });
    let slot: Arc<AtomicBool> = Arc::default();
    let now = Rc::new(Cell::new(Duration::ZERO));
    let notices = FakeNotices::devices_at(&now, Duration::ZERO);
    let mut first = rebind_for("Mic (Box)", id("{a}"), blocked.clone(), (Run::Thread, slot.clone())).unwrap();
    assert!(first.check(&notices).is_none());
    let deadline = Instant::now() + Duration::from_secs(5);
    while started.load(Ordering::SeqCst) == 0 && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(5));
    }
    // The stream ended with its re-pick in flight; the reopened stream gets a notice.
    drop(first);
    let mut second = rebind_for("Mic (Box)", id("{a}"), blocked, (Run::Thread, slot.clone())).unwrap();
    second.queued = true;
    for _ in 0..5 {
        assert!(second.check(&notices).is_none());
    }
    std::thread::sleep(Duration::from_millis(50));
    assert_eq!(started.load(Ordering::SeqCst), 1, "one re-pick thread at a time");
    assert!(second.queued, "the skipped notice waits for the slot");
    release.send(()).unwrap();
    release.send(()).unwrap();
    let mut ended = None;
    while ended.is_none() && Instant::now() < deadline {
        ended = second.check(&notices);
        std::thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(started.load(Ordering::SeqCst), 2, "the queued notice ran once the slot was free");
    assert!(ended.is_some(), "and its answer came back");
}
