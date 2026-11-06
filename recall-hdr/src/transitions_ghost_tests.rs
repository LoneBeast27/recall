//! Transition matrix row U3 (third audit): a borderless game hangs ("Not Responding") and
//! the user clicks it, so Windows' DWM ghost window (likely: owned by `dwm.exe`, same rect,
//! class `Ghost`) takes the foreground. Driven through `transitions_sim::Sim`; the ghost's
//! event is what `watcher::build_event` makes after `ghost_window::resolve` (injected
//! class / hung-window lookups, no real hang).

use crate::ghost_window::{resolve_with, GHOST_CLASS};
use crate::transitions_sim::{app_ev, game_ev, Sim};
use crate::watcher::WatchEvent;

const VAL: &str = "VALORANT-Win64-Shipping";
const GHOST: isize = 0x99;

/// The watcher's event while the ghost is foreground. `found` = user32 found the hung window.
fn ghost_focus(found: bool) -> WatchEvent {
    let class_of = |h: isize| (h == GHOST).then(|| GHOST_CLASS.to_string());
    match resolve_with(GHOST, class_of, |_| if found { 0x10 } else { 0 }) {
        0x10 => game_ev(VAL, 10, 0x10),
        _ => app_ev("dwm", 500, GHOST, true),
    }
}

/// Expected: the hang and the recovery are invisible to the session: same target, 0 engine
/// restarts, ring kept (the clip of the freeze is still there), no drain, no notice.
fn hang_then_recover(found: bool) {
    let mut s = Sim::new();
    s.focus(game_ev(VAL, 10, 0x10));
    s.run_to(400);
    assert!(s.recording_of(10), "setup: the game is recorded");
    let mark = s.now;
    for at in (1_000..=11_000).step_by(1_000) {
        s.run_to(at);
        s.focus(ghost_focus(found)); // hook, then the 1 s re-probe
    }
    s.run_to(12_000);
    s.focus(game_ev(VAL, 10, 0x10)); // the game recovers, the ghost is destroyed
    s.run_to(20_000);
    assert_eq!(s.current().map(|t| t.pid), Some(10));
    assert!(s.ring_kept_since(mark), "0 restarts, ring kept");
    assert!(s.planner.drain().is_none());
    assert_eq!(s.game_notices("dwm"), 0);
}

#[test]
fn u3_a_hung_game_s_ghost_window_keeps_the_session() {
    hang_then_recover(true);
}

#[test]
fn u3_an_unresolved_ghost_is_dwm_and_is_never_a_target() {
    hang_then_recover(false);
}
