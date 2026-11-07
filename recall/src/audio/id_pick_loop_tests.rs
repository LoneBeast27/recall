//! A4 amendment (merge gate, 08:29 UTC) through the reopen loop: the endpoint-id tie-break picks
//! the source's own device among same-named endpoints, and Alt+M still decides by the id alone.
//! A muted mic never records in any of these. Made-up names and ids, no WASAPI.

use super::tests::{ring, FakeClock, FakeNotices};
use super::*;
use crate::audio::endpoints::{opened_own, pick_endpoint};
use std::cell::Cell;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};

/// One open endpoint that plays 0.5 for a second while Alt+M is on.
struct Playing {
    now: Rc<Cell<Duration>>,
    endpoint: String,
    own: bool,
}

impl Capture for Playing {
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

/// What a source saved as (`saved`, `saved_id`) records while Alt+M is on, with `present`
/// (name, id) connected: (the endpoint it opened, the samples held). `mic` = a microphone
/// (`mic_mute = true`), else a line-in (`mic_mute = false`).
fn recorded(mic: bool, saved: &str, saved_id: &str, present: &[(&str, &str)]) -> (Option<String>, Vec<f32>) {
    let ring = TrackRing { mic_mute: AtomicBool::new(mic), on_default: false, ..ring() };
    let names: Vec<String> = present.iter().map(|(n, _)| (*n).to_owned()).collect();
    let now = Rc::new(Cell::new(Duration::ZERO));
    let mut clock = FakeClock { now: now.clone(), stop_at: Duration::from_millis(1_500) };
    let (tx, _rx) = std::sync::mpsc::channel();
    let opened = std::cell::RefCell::new(None);
    let open = |_reopen: bool| {
        // As `stream::pick_device` decides it, on the injected endpoints and ids.
        let Some(i) = pick_endpoint(&names, saved, saved_id, |i| Some(present[i].1.to_owned())) else { return Err("not connected".to_owned()) };
        if opened.borrow().is_some() {
            return Err("lost".to_owned());
        }
        *opened.borrow_mut() = Some(present[i].0.to_owned());
        let own = opened_own(Some(present[i].1), saved, saved_id);
        Ok(Playing { now: now.clone(), endpoint: present[i].0.to_owned(), own })
    };
    capture_loop(&ring, saved, true, open, tx, &mut clock, &FakeNotices::none(&now));
    assert!(!ring.opened_own.load(Ordering::Relaxed), "not own once the stream ended");
    let held = ring.samples.lock().unwrap().iter().copied().collect();
    (opened.into_inner(), held)
}

fn live(samples: &[f32]) -> bool {
    samples.contains(&0.5)
}

const RENUMBERED: [(&str, &str); 2] = [("4- Line (Box)", "{sub}"), ("5- Line (Box)", "{own}")];
const TWINS: [(&str, &str); 2] = [("Line (Box)", "{sub}"), ("Line (Box)", "{own}")];
const NO_MATCH: [(&str, &str); 2] = [("4- Line (Box)", "{a}"), ("5- Line (Box)", "{b}")];
const ID_ELSEWHERE: [(&str, &str); 2] = [("Line (Desk)", "{own}"), ("Speakers (Box)", "{spk}")];

#[test]
fn a_line_in_back_renumbered_beside_a_substitute_records_its_own_device() {
    let (opened, held) = recorded(false, "3- Line (Box)", "{own}", &RENUMBERED);
    assert_eq!(opened.as_deref(), Some("5- Line (Box)"), "its own device, picked by the id");
    assert!(live(&held), "own: live through Alt+M");
    let (opened, held) = recorded(false, "Line (Box)", "{own}", &TWINS);
    assert_eq!(opened.as_deref(), Some("Line (Box)"));
    assert!(live(&held), "the own one of two exact twins: live through Alt+M");
}

#[test]
fn same_named_devices_without_an_id_match_record_nothing() {
    for (saved, saved_id, present, why) in [
        ("3- Line (Box)", "{own}", &NO_MATCH, "the id is neither: waits"),
        ("3- Line (Box)", "", &NO_MATCH, "no saved id: waits"),
        ("Line (Box)", "{own}", &ID_ELSEWHERE, "an id match with another name never opens"),
    ] {
        let (opened, held) = recorded(false, saved, saved_id, present);
        assert_eq!(opened, None, "{why}");
        assert!(!live(&held), "{why}: silent");
    }
}

#[test]
fn a_muted_mic_never_records_in_any_of_these() {
    for (saved, present) in [("3- Line (Box)", &RENUMBERED), ("Line (Box)", &TWINS), ("3- Line (Box)", &NO_MATCH), ("Line (Box)", &ID_ELSEWHERE)] {
        let (_, held) = recorded(true, saved, "{own}", present);
        assert!(!live(&held), "a microphone saved as {saved:?} stays muted by Alt+M on {present:?}");
    }
}
