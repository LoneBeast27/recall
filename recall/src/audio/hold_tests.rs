//! A4 ruling (merge gate p9) through the reopen loop: an input opened on an unsure exact-name
//! sibling records silence whatever Alt+M says, until the first press after it opened that
//! leaves the mic unmuted; only then is its id offered as the source's own. Fake stream, clock
//! and Alt+M (no WASAPI, no global mute state); made-up names and ids.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::atomic::AtomicBool;
use std::time::Duration;

use crate::audio::apps::Leveller;
use crate::audio::capture_loop::tests::{ring, FakeClock, FakeNotices};
use crate::audio::capture_loop::{capture_loop, Capture, Clock, Ended, Notices};
use crate::audio::endpoints::{choose_endpoint_by, Pick};
use crate::audio::mute::MuteNow;
use crate::audio::TrackRing;

/// The connected endpoints: B took the saved name, C is "2- Mic (Box)".
const PRESENT: [(&str, &str); 2] = [("Mic (Box)", "{B}"), ("2- Mic (Box)", "{C}")];

/// Plays 0.5 in 20 ms packets; packet n sees Alt+M as `alt_m(n)`. The status after each packet
/// goes to `status`.
struct Unsure {
    now: Rc<Cell<Duration>>,
    unsure: Option<Option<String>>,
    alt_m: fn(usize) -> MuteNow,
    status: Rc<RefCell<Vec<String>>>,
}

impl Capture for Unsure {
    fn endpoint(&self) -> String {
        "Mic (Box)".into()
    }

    fn unsure(&self) -> Option<Option<String>> {
        self.unsure.clone()
    }

    fn run(&mut self, ring: &TrackRing, level: &mut Leveller, _clock: &dyn Clock, _notices: &dyn Notices) -> Ended {
        for n in 0..50 {
            self.now.set(self.now.get() + Duration::from_millis(20));
            ring.push_live(&level.live(self.now.get(), &[0.5; 2], 1), (self.alt_m)(n));
            self.status.borrow_mut().push(ring.endpoint());
        }
        Ended { why: "device lost".into(), at_once: false }
    }
}

/// What a source saved as "Mic (Box)" with id A records (per 20 ms packet: live or not), with
/// Alt+M per packet `alt_m`; `mic` = a microphone, else a line-in. Also the status per packet
/// and the id offered as its own at the end.
fn run(mic: bool, alt_m: fn(usize) -> MuteNow) -> (Vec<bool>, Vec<String>, Option<String>) {
    let ring = TrackRing { mic_mute: AtomicBool::new(mic), on_default: false, ..ring() };
    let names: Vec<String> = PRESENT.iter().map(|(n, _)| (*n).to_owned()).collect();
    let now = Rc::new(Cell::new(Duration::ZERO));
    let mut clock = FakeClock { now: now.clone(), stop_at: Duration::from_millis(1_100) };
    let status = Rc::new(RefCell::new(Vec::new()));
    let (tx, _rx) = std::sync::mpsc::channel();
    let opens = Cell::new(0);
    let open = |_reopen: bool| {
        // As `stream::pick_device` decides it, on the injected endpoints.
        let pick = choose_endpoint_by(&names, (Some("Mic (Box)"), "{A}"), (false, true), |i| Some(PRESENT[i].1.to_owned()))?;
        if opens.replace(opens.get() + 1) > 0 {
            return Err("lost".to_owned());
        }
        let unsure = match pick {
            Pick::Unsure(i) => Some(Some(PRESENT[i].1.to_owned())),
            _ => None,
        };
        Ok(Unsure { now: now.clone(), unsure, alt_m, status: status.clone() })
    };
    capture_loop(&ring, "Mic (Box)", true, open, tx, &mut clock, &FakeNotices::none(&now));
    assert!(opens.get() > 0, "the source opened");
    let held: Vec<f32> = ring.samples.lock().unwrap().iter().copied().collect();
    // One frame per 10 ms, mono: two samples per packet.
    let packets = held.chunks(2).map(|c| c.contains(&0.5)).collect();
    let status = status.borrow().clone();
    (packets, status, ring.live.adopted())
}

fn unmuted_no_press(_: usize) -> MuteNow {
    MuteNow { muted: false, presses: 7 }
}

/// Unmuted at open; Alt+M at packet 10 (muted), again at packet 20 (unmuted).
fn press_twice(n: usize) -> MuteNow {
    match n {
        0..=9 => MuteNow { muted: false, presses: 7 },
        10..=19 => MuteNow { muted: true, presses: 8 },
        _ => MuteNow { muted: false, presses: 9 },
    }
}

/// Muted at open; Alt+M at packet 10 unmutes.
fn muted_then_press(n: usize) -> MuteNow {
    if n < 10 {
        MuteNow { muted: true, presses: 3 }
    } else {
        MuteNow { muted: false, presses: 4 }
    }
}

#[test]
fn the_unsure_sibling_opens_muted_whatever_alt_m_says() {
    for mic in [true, false] {
        let (packets, status, adopted) = run(mic, unmuted_no_press);
        assert!(packets.iter().all(|live| !live), "mic={mic}: B is silent with Alt+M off and no press");
        assert_eq!(status[0], "Mic (Box) may be a different device (its ID changed). Started muted; press Alt+M to unmute.");
        assert_eq!(adopted, None, "no press: B's id is not saved");
    }
}

#[test]
fn only_a_fresh_alt_m_after_it_opens_unmutes_it_and_then_its_id_is_saved() {
    // A microphone, which follows Alt+M: adoption stays as it was.
    let (packets, status, adopted) = run(true, muted_then_press);
    assert!(packets.iter().any(|&live| live), "unmuted by the press");
    assert_eq!(adopted.as_deref(), Some("{B}"), "B's id is offered as its own");
    assert_eq!(status[5], "Mic (Box) may be a different device (its ID changed). Started muted; press Alt+M to unmute.");
    assert_eq!(status.last().map(String::as_str), Some("Mic (Box)"), "the notice is gone");
    // A press that mutes keeps it held; the next press that unmutes frees it.
    let (packets, status, adopted) = run(true, press_twice);
    assert!(packets.iter().any(|&live| live), "free after the unmuting press");
    assert!(status[15].starts_with("Mic (Box) may be"), "still held after the muting press");
    assert_eq!(adopted.as_deref(), Some("{B}"));
}

#[test]
fn the_hold_ends_with_its_stream() {
    let ring = ring();
    ring.live.start_hold("Mic (Box)", Some("{B}".into()));
    ring.live.end_hold();
    ring.push_live(&[0.5, 0.5], MuteNow { muted: false, presses: 0 });
    let held: Vec<f32> = ring.samples.lock().unwrap().iter().copied().collect();
    assert!(held.contains(&0.5), "a stream after the held one starts free (the next open decides again)");
    assert_eq!(ring.live.adopted(), None);
}

#[test]
fn a_saved_id_sent_live_reopens_the_stream_once() {
    let ring = ring();
    ring.live.set_saved_id("{B}");
    assert!(ring.live.take_repick());
    assert!(!ring.live.take_repick(), "taken once");
    ring.live.set_saved_id(" {B} ");
    assert!(!ring.live.take_repick(), "the same id again changes nothing");
    assert_eq!(ring.live.saved_id(), "{B}");
}

/// P1-b, two identical dongles: a line-in saved with id A, B (the exact name) and C connected.
/// An adopted id would make B "own", and Alt+M would never mute it again: no id is ever
/// adopted for a line-in. B stays muted and the status says to pick it on the Audio page.
#[test]
fn a_line_in_sibling_stays_muted_after_alt_m_and_adopts_nothing() {
    for alt_m in [muted_then_press, press_twice] {
        let (packets, status, adopted) = run(false, alt_m);
        assert!(packets.iter().all(|live| !live), "B stays muted through the unmuting press");
        assert_eq!(adopted, None, "no endpoint id is adopted for a line-in");
        assert_eq!(status[5], "Mic (Box) may be a different device (its ID changed). Started muted; press Alt+M to unmute.");
        assert_eq!(
            status.last().map(String::as_str),
            Some("Mic (Box) may be a different device (its ID changed). Line-in stays muted: pick it on the Audio page."),
            "the notice tells the user to pick the device"
        );
    }
}
