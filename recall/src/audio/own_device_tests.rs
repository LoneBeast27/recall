//! R5-deep-audio-1: a line-in (`mic_mute = false`, a named device) skips Alt+M only while the
//! stream is its own saved device: the exact saved name and, when one was saved, the same
//! endpoint id. Endpoint lists and ids are injected (made-up names, no WASAPI).

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
        let own = opened_own(present[i].0, Some(present[i].1), saved, saved_id);
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
    let legacy = recorded("Mic (Box)", "", &[("Mic (Box)", "{id-one}")]);
    assert!(live(&legacy), "an old config without an id: an exact-name open keeps its sound");
}

#[test]
fn a_twin_that_took_the_saved_name_follows_the_mic_hotkey() {
    let twin = recorded("Mic (Box)", "{id-one}", &[("Mic (Box)", "{id-two}")]);
    assert!(!live(&twin), "exact name, another endpoint id: muted by Alt+M");
}

/// The rule itself (the Audio page's copy in `recall-native` `meters.rs` tests the same table).
#[test]
fn opened_own_needs_the_exact_name_and_the_saved_id() {
    let table = [
        ("Mic (Box)", Some("{a}"), "Mic (Box)", "{a}", true),
        ("Mic (Box)", Some("{a}"), " Mic (Box) ", "", true),
        ("Mic (Box)", Some("{b}"), "Mic (Box)", "{a}", false),
        ("Mic (Box)", None, "Mic (Box)", "{a}", false),
        ("2- Mic (Box)", Some("{a}"), "Mic (Box)", "{a}", false),
        ("Mic (Box)", Some("{a}"), "3- Mic (Box)", "", false),
        ("Mic (Box)", Some("{a}"), "", "", false),
    ];
    for (opened, id, want, want_id, own) in table {
        assert_eq!(opened_own(opened, id, want, want_id), own, "{opened:?} {id:?} for {want:?} {want_id:?}");
    }
}
