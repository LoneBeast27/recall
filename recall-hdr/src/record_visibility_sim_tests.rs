//! U5 against the transition harness (`transitions_sim::Sim`): the hidden-window
//! rule runs before the tracker poll and the engine plan, as `check_windows` does
//! in `record_app::tick`. Every engine `Apply` is a new replay ring.

use std::cell::Cell;
use std::time::Duration;

use super::*;
use crate::transitions_sim::{game_ev, Sim, TICK};
use crate::watcher::WatchEvent;

const PID: u32 = 10;
const GAME: isize = 0x10;

fn game() -> WatchEvent {
    game_ev("game", PID, GAME)
}

/// A Sim recording the game; returns it with the engine-start time.
fn recording() -> Sim {
    let mut s = Sim::new();
    s.focus(game());
    s.run_to(1_000);
    assert!(s.recording_of(PID), "the game records");
    s
}

/// One pump tick: the hidden-window rule, then the tracker and the planner.
fn tick(s: &mut Sim, w: &mut HiddenWatch, state: Shown) {
    s.now += TICK;
    let now = s.now;
    drop_hidden(w, &mut s.tracker, now, |_| state);
    s.tick();
}

#[test]
fn a_hung_window_behind_its_ghost_keeps_the_session_for_30_s() {
    // A shader compile: the game stops pumping, DWM hides it and shows a ghost;
    // the ghost's foreground events map back to the hung window (ghost_window).
    let mut s = recording();
    let mark = s.now;
    let mut w = HiddenWatch::default();
    let end = mark + Duration::from_secs(30);
    let mut next_ghost = mark;
    while s.now < end {
        if s.now >= next_ghost {
            s.focus(game()); // the ghost, mapped to the hung window
            next_ghost += Duration::from_millis(1300);
        }
        tick(&mut s, &mut w, Shown::Hung);
        assert_eq!(s.current().map(|t| t.hwnd), Some(GAME), "session kept at {:?}", s.now - mark);
    }
    assert_eq!(s.spawns_since(mark), 0, "no engine restart");
    assert!(s.ring_kept_since(mark), "the replay ring is the same one");
}

#[test]
fn hidden_3_s_then_shown_with_a_new_title_keeps_the_engine_and_ring() {
    // A device re-create or a borderless <-> exclusive restyle hides the window
    // for a few seconds; it comes back titled "Game - Loading".
    let mut s = recording();
    let mark = s.now;
    let mut w = HiddenWatch::default();
    let shown_at = mark + Duration::from_secs(3);
    while s.now < shown_at {
        tick(&mut s, &mut w, Shown::Hidden);
    }
    s.focus(WatchEvent { title: "Game - Loading".into(), ..game() });
    let end = s.now + Duration::from_secs(5);
    while s.now < end {
        tick(&mut s, &mut w, Shown::Visible);
    }
    assert_eq!(s.current().map(|t| t.hwnd), Some(GAME));
    assert_eq!(s.spawns_since(mark), 0, "no engine restart");
    assert!(s.ring_kept_since(mark), "the replay ring is the same one");
}

#[test]
fn hidden_past_the_grace_still_ends_the_session() {
    let mut s = recording();
    let mut w = HiddenWatch::default();
    let ends = Cell::new(None);
    let mark = s.now;
    while s.now < mark + HIDDEN_FOR + Duration::from_secs(1) {
        tick(&mut s, &mut w, Shown::Hidden);
        if ends.get().is_none() && s.current().is_none() {
            ends.set(Some(s.now - mark));
        }
    }
    let at = ends.get().expect("the session ended");
    assert!(at > HIDDEN_FOR, "not before the grace: {at:?}");
}
