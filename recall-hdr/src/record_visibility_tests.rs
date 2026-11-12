//! U5: a hidden session window ends the session. The window state and time
//! are injected; nothing touches the OS.

use std::cell::Cell;
use std::time::{Duration, Instant};

use super::*;
use crate::record_target::{Target, TargetTracker, START_DEBOUNCE, TARGET_DEBOUNCE};

const GAME: isize = 0x10;

fn ms(n: u64) -> Duration {
    Duration::from_millis(n)
}

/// A tracker recording the game (pid 1, window GAME), committed at `t0`.
fn live(t0: Instant) -> (TargetTracker, Instant) {
    let mut tr = TargetTracker::new(TARGET_DEBOUNCE, START_DEBOUNCE);
    let game = Target { hwnd: GAME, pid: 1, name: "Game".into(), exe: "game.exe".into() };
    tr.on_foreground(Some(game), t0);
    let at = t0 + START_DEBOUNCE;
    assert!(tr.poll(at));
    (tr, at)
}

/// Pump ticks every 50 ms from `from` for `len`, reading `state` each tick;
/// returns the first window dropped.
fn run(watch: &mut HiddenWatch, tr: &mut TargetTracker, from: Instant, len: Duration, state: &Cell<Shown>) -> Option<isize> {
    let mut at = from;
    while at <= from + len {
        if let Some(h) = drop_hidden(watch, tr, at, |_| state.get()) {
            return Some(h);
        }
        at += ms(50);
    }
    None
}

#[test]
fn a_window_hidden_for_over_5_s_ends_the_session_and_its_process_stays_adoptable() {
    let (mut tr, t0) = live(Instant::now());
    let mut w = HiddenWatch::default();
    let state = Cell::new(Shown::Hidden);
    assert_eq!(run(&mut w, &mut tr, t0, ms(5500), &state), Some(GAME), "hidden 5.5 s: gone");
    assert!(tr.current().is_none(), "the session ended (the drain starts)");
    assert!(tr.adopts(1, t0 + ms(5600)), "T5a: the game's next window is the session again");
}

#[test]
fn a_minimized_window_keeps_the_session() {
    let (mut tr, t0) = live(Instant::now());
    let mut w = HiddenWatch::default();
    let state = Cell::new(Shown::Minimized);
    assert_eq!(run(&mut w, &mut tr, t0, ms(5000), &state), None);
    assert_eq!(tr.current().map(|t| t.hwnd), Some(GAME));
}

#[test]
fn a_window_cloaked_by_a_desktop_switch_keeps_the_session() {
    let (mut tr, t0) = live(Instant::now());
    let mut w = HiddenWatch::default();
    let state = Cell::new(Shown::Cloaked);
    assert_eq!(run(&mut w, &mut tr, t0, ms(5000), &state), None);
    assert_eq!(tr.current().map(|t| t.hwnd), Some(GAME));
}

#[test]
fn hidden_for_half_a_second_then_shown_again_keeps_the_session() {
    let (mut tr, t0) = live(Instant::now());
    let mut w = HiddenWatch::default();
    let state = Cell::new(Shown::Hidden);
    assert_eq!(run(&mut w, &mut tr, t0, ms(500), &state), None, "0.5 s hidden");
    state.set(Shown::Visible);
    assert_eq!(run(&mut w, &mut tr, t0 + ms(550), ms(100), &state), None, "shown again");
    // Hidden again: the clock starts over, so 0.9 s more is still not gone.
    state.set(Shown::Hidden);
    assert_eq!(run(&mut w, &mut tr, t0 + ms(700), ms(900), &state), None, "a fresh 0.9 s");
    assert_eq!(tr.current().map(|t| t.hwnd), Some(GAME));
}

#[test]
fn no_session_reads_nothing() {
    let mut w = HiddenWatch::default();
    let reads = Cell::new(0);
    let probe = |_| {
        reads.set(reads.get() + 1);
        Shown::Hidden
    };
    assert_eq!(w.step(None, Instant::now(), probe), None);
    assert_eq!(reads.get(), 0);
}

/// With no 50 ms tick: one pass at the hide event, the next at the watch's deadline,
/// and the session ends there (two reads, not a hundred).
#[test]
fn the_hidden_rule_needs_only_the_hide_event_and_its_deadline() {
    let t0 = Instant::now();
    let (mut tr, at) = live(t0);
    let mut watch = HiddenWatch::default();
    assert_eq!(watch.deadline(), None, "nothing hidden: nothing to wake for");
    let reads = Cell::new(0);
    let hidden = |_| {
        reads.set(reads.get() + 1);
        Shown::Hidden
    };
    let hide = at + ms(700);
    assert_eq!(drop_hidden(&mut watch, &mut tr, hide, hidden), None, "the hide event's pass starts the clock");
    let due = watch.deadline().expect("a deadline for the pump");
    assert!(due > hide + HIDDEN_FOR && due <= hide + HIDDEN_FOR + ms(1), "{:?}", due - hide);
    assert_eq!(drop_hidden(&mut watch, &mut tr, due, hidden), Some(GAME), "gone at the deadline");
    assert_eq!(reads.get(), 2);
    assert_eq!(watch.deadline(), None);
}

/// Hide, show, hide (a display-mode change): the show's pass resets the clock, so the
/// session ends 5 s after the second hide, not the first.
#[test]
fn a_show_between_two_hides_restarts_the_hidden_clock() {
    let t0 = Instant::now();
    let (mut tr, at) = live(t0);
    let mut watch = HiddenWatch::default();
    let hidden = |_| Shown::Hidden;
    let first = at + ms(100);
    assert_eq!(drop_hidden(&mut watch, &mut tr, first, hidden), None, "the first hide");
    assert_eq!(drop_hidden(&mut watch, &mut tr, first + ms(400), |_| Shown::Visible), None, "the show event's pass");
    assert_eq!(watch.deadline(), None, "shown: the clock is gone");
    let second = first + ms(800);
    assert_eq!(drop_hidden(&mut watch, &mut tr, second, hidden), None, "the second hide starts it again");
    assert_eq!(drop_hidden(&mut watch, &mut tr, first + HIDDEN_FOR + ms(100), hidden), None, "5 s after the first hide: kept");
    let due = watch.deadline().expect("a deadline");
    assert!(due > second + HIDDEN_FOR, "counted from the second hide");
    assert_eq!(drop_hidden(&mut watch, &mut tr, due, hidden), Some(GAME));
}
