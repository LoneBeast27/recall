//! R5/R6-deep-audio-1: a line-in (`mic_mute = false`, a named device) skips Alt+M only while the
//! stream is its own saved device: the endpoint id saved at pick time, whatever the name; no id
//! saved = it follows Alt+M. Endpoint lists and ids are injected (made-up names, no WASAPI).

use super::tests::{ring, FakeClock, FakeNotices};
use super::*;
use crate::audio::endpoints::{choose_endpoint, opened_own, Pick};
use std::cell::Cell;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};

/// One open endpoint that plays 0.5 for a second while Alt+M is on.
struct MutedStream {
    now: Rc<Cell<Duration>>,
    endpoint: String,
    own: bool,
}

impl Capture for MutedStream {
    fn endpoint(&self) -> String {
        self.endpoint.clone()
    }

    fn own(&self) -> bool {
        self.own
    }

    fn run(&mut self, ring: &TrackRing, level: &mut Leveller, _clock: &dyn Clock, _notices: &dyn Notices) -> Ended {
        for _ in 0..50 {
            self.now.set(self.now.get() + Duration::from_millis(20));
            ring.push_capture(&level.live(self.now.get(), &[0.5; 2], 1), true); // Alt+M is on
        }
        Ended { why: "device lost".into(), at_once: false }
    }
}

/// What a line-in saved as (`saved`, `saved_id`) records while Alt+M is on, when the connected
/// endpoints are `present` (name, id): the samples held after one open through `capture_loop`.
fn recorded(saved: &str, saved_id: &str, present: &[(&str, &str)]) -> Vec<f32> {
    let ring = TrackRing { mic_mute: AtomicBool::new(false), on_default: false, ..ring() };
    let names: Vec<String> = present.iter().map(|(n, _)| (*n).to_owned()).collect();
    let now = Rc::new(Cell::new(Duration::ZERO));
    let mut clock = FakeClock { now: now.clone(), stop_at: Duration::from_millis(1_500) };
    let (tx, _rx) = std::sync::mpsc::channel();
    let opened = Cell::new(false);
    let open = |_reopen: bool| {
        // As `stream::pick_device` decides it, on the injected endpoints.
        let Ok(Pick::Named(i)) = choose_endpoint(&names, Some(saved), false) else { return Err("not connected".to_owned()) };
        if opened.replace(true) {
            return Err("lost".to_owned());
        }
        let own = opened_own(Some(present[i].1), saved, saved_id);
        Ok(MutedStream { now: now.clone(), endpoint: present[i].0.to_owned(), own })
    };
    capture_loop(&ring, saved, true, open, tx, &mut clock, &FakeNotices::none(&now));
    assert!(opened.get(), "an endpoint opened for {saved:?}");
    assert!(!ring.opened_own.load(Ordering::Relaxed), "not own once the stream ended");
    let held = ring.samples.lock().unwrap().iter().copied().collect();
    held
}

fn live(samples: &[f32]) -> bool {
    samples.contains(&0.5)
}

#[test]
fn a_line_in_on_a_renumbered_fallback_follows_the_mic_hotkey() {
    let other = recorded("Mic (Box)", "", &[("2- Mic (Box)", "{id-two}")]);
    assert!(!live(&other), "\"Mic (Box)\" saved, only \"2- Mic (Box)\" present: opened by the fallback, muted by Alt+M");
    let other_with_id = recorded("Mic (Box)", "{id-one}", &[("Speakers (Box)", "{id-spk}"), ("2- Mic (Box)", "{id-two}")]);
    assert!(!live(&other_with_id), "a saved id does not make the fallback its own");
}

#[test]
fn a_line_in_on_its_own_device_keeps_its_sound() {
    let own = recorded("Mic (Box)", "{id-one}", &[("Mic (Box)", "{id-one}"), ("Speakers (Box)", "{id-spk}")]);
    assert!(live(&own), "exact name and matching id: live through Alt+M");
}

/// R6-deep-audio-1: no saved id, no claim. Windows can give the saved name to the other
/// identical dongle (the headset), so an exact name alone follows Alt+M.
#[test]
fn a_line_in_without_a_saved_id_follows_the_mic_hotkey() {
    let legacy = recorded("Mic (Box)", "", &[("Mic (Box)", "{id-one}")]);
    assert!(!live(&legacy), "no id saved, exact name: muted by Alt+M");
}

/// R6-deep-audio-2: the saved id is the identity, whatever Windows calls the endpoint now.
#[test]
fn the_saved_id_alone_decides_its_own_device() {
    let renumbered = recorded("3- Line (Box)", "{a}", &[("4- Line (Box)", "{a}")]);
    assert!(live(&renumbered), "renumbered \"4- Line (Box)\", same id: live through Alt+M");
    let other = recorded("3- Line (Box)", "{a}", &[("4- Line (Box)", "{b}")]);
    assert!(!live(&other), "renumbered, another id: muted by Alt+M");
}

/// R6-deep-audio-5: a hand-edited id that is not one line of at most 256 characters counts as
/// no id: the source still opens and records, and follows Alt+M.
#[test]
fn a_malformed_saved_id_counts_as_none() {
    let long = "x".repeat(300);
    assert!(!live(&recorded("Mic (Box)", &long, &[("Mic (Box)", &long)])), "an id over 256 characters: muted");
    assert!(!live(&recorded("Mic (Box)", "{a}\u{7}", &[("Mic (Box)", "{a}\u{7}")])), "a control character: muted");
}

#[test]
fn a_twin_that_took_the_saved_name_follows_the_mic_hotkey() {
    let twin = recorded("Mic (Box)", "{id-one}", &[("Mic (Box)", "{id-two}")]);
    assert!(!live(&twin), "exact name, another endpoint id: muted by Alt+M");
}

/// Plays 0.5 in 20 ms packets while Alt+M is on, checking its rebind before every packet.
struct Rebinding {
    now: Rc<Cell<Duration>>,
    endpoint: &'static str,
    own: bool,
    rebind: Option<Rebind>,
}

impl Capture for Rebinding {
    fn endpoint(&self) -> String {
        self.endpoint.to_owned()
    }

    fn own(&self) -> bool {
        self.own
    }

    fn run(&mut self, ring: &TrackRing, level: &mut Leveller, _clock: &dyn Clock, notices: &dyn Notices) -> Ended {
        for _ in 0..100 {
            if let Some(ended) = self.rebind.as_ref().and_then(|r| r.check(notices)) {
                return ended;
            }
            self.now.set(self.now.get() + Duration::from_millis(20));
            ring.push_capture(&level.live(self.now.get(), &[0.5; 2], 1), true); // Alt+M is on
        }
        Ended { why: "device lost".into(), at_once: false }
    }
}

/// R6-deep-audio-3: a line-in opened on a fallback ("2- Mic (Box)", its own dongle unplugged)
/// goes back to its own device when that returns (a device notice at 0.5 s): a second open, own,
/// so it records live through Alt+M from then on. Before, it stayed on the fallback.
#[test]
fn a_fallback_stream_goes_back_to_its_own_device_when_it_returns() {
    let ring = TrackRing { mic_mute: AtomicBool::new(false), on_default: false, ..ring() };
    let now = Rc::new(Cell::new(Duration::ZERO));
    let mut clock = FakeClock { now: now.clone(), stop_at: Duration::from_millis(1_500) };
    let notices = FakeNotices::devices_at(&now, Duration::from_millis(500));
    let opens = Cell::new(0);
    let (tx, _rx) = std::sync::mpsc::channel();
    let open = |_reopen: bool| {
        opens.set(opens.get() + 1);
        let back = now.clone();
        Ok::<_, String>(match opens.get() {
            1 => Rebinding { now: now.clone(), endpoint: "2- Mic (Box)", own: false, rebind: rebind_for("Mic (Box)", false, move || back.get() >= Duration::from_millis(500)) },
            _ => Rebinding { now: now.clone(), endpoint: "Mic (Box)", own: true, rebind: None },
        })
    };
    capture_loop(&ring, "Mic (Box)", true, open, tx, &mut clock, &notices);
    assert_eq!(opens.get(), 2, "the fallback ended when its own device came back, and it reopened");
    let held: Vec<f32> = ring.samples.lock().unwrap().iter().copied().collect();
    assert!(!held[..40].contains(&0.5), "muted on the fallback");
    assert!(held[60..].contains(&0.5), "live on its own device");
}

/// The rule itself (the Audio page's copy in `recall-native` `meters.rs` tests the same table).
#[test]
fn opened_own_is_the_saved_id_alone() {
    let long = "x".repeat(257);
    let table = [
        (Some("{a}"), "Mic (Box)", "{a}", true),
        (Some(" {a} "), "3- Mic (Box)", "{a}", true),
        (Some("{a}"), "Mic (Box)", "", false),
        (Some("{b}"), "Mic (Box)", "{a}", false),
        (None, "Mic (Box)", "{a}", false),
        (Some("{a}"), "", "{a}", false),
        (Some(long.as_str()), "Mic (Box)", long.as_str(), false),
        (Some("{a}\nb"), "Mic (Box)", "{a}\nb", false),
    ];
    for (id, want, want_id, own) in table {
        assert_eq!(opened_own(id, want, want_id), own, "{id:?} for {want:?} {want_id:?}");
    }
}
