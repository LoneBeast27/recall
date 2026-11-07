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
fn a_window_hidden_for_1_5_s_ends_the_session_and_its_process_stays_adoptable() {
    let (mut tr, t0) = live(Instant::now());
    let mut w = HiddenWatch::default();
    let state = Cell::new(Shown::Hidden);
    assert_eq!(run(&mut w, &mut tr, t0, ms(1500), &state), Some(GAME), "hidden 1.5 s: gone");
    assert!(tr.current().is_none(), "the session ended (the drain starts)");
    assert!(tr.adopts(1, t0 + ms(1600)), "T5a: the game's next window is the session again");
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
