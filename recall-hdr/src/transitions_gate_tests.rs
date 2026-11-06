//! Merge-gate rows for fix/transition-targets-v2 (2026-11-07): same-process adoption keeps
//! the size floor and names the dropped clip, the session-ends tick holds a due window,
//! a `fullscreen_games` entry waits for a drain, a long recording stopped by a window that
//! waited for the drain says "the game closed", and a pin is not a dropped clip. Same harness
//! as `transitions_scope_tests` (the controller's own `record_transition` rules).

use crate::record_activity::Trigger;
use crate::record_engine::EngineTarget;
use crate::record_target::Target;
use crate::transitions_sim::{app_ev, game_ev, steam_ev, Sim};

const VAL: &str = "VALORANT-Win64-Shipping";
/// A game Recall knows only by its fullscreen window (not Steam, no override, not known).
const UNITY: &str = "UnityGame";

fn recording(stem: &str) -> Sim {
    let mut s = Sim::new();
    s.focus(game_ev(stem, 10, 0x10));
    s.run_to(400);
    assert!(s.recording_of(10), "setup: the game is recorded");
    s
}

/// The game's drain running from t = 5 s (its process exited).
fn draining() -> Sim {
    let mut s = recording(VAL);
    s.run_to(5_000);
    s.exit(10);
    s.run_to(5_100);
    assert!(s.planner.drain().is_some(), "setup: the game drains");
    s
}

fn drain_end(s: &Sim) -> u64 {
    let (_, at, len) = s.planner.drain().expect("a drain");
    (at + len).duration_since(s.t0).as_millis() as u64
}

fn engine_hwnd(s: &Sim) -> Option<isize> {
    match s.engine() {
        EngineTarget::Window(t) => Some(t.hwnd),
        _ => None,
    }
}

/// P1a: the session's window is destroyed and the same process shows a small window (an
/// error MessageBox, an in-process launcher splash). Expected: not adopted, the drain runs.
#[test]
fn a_small_window_of_the_game_is_not_adopted() {
    let mut s = recording(UNITY);
    s.run_to(5_000);
    s.window_gone(0x10);
    s.client = (420, 180);
    s.focus(app_ev(UNITY, 10, 0x11, false));
    s.run_to(9_000);
    assert_eq!(engine_hwnd(&s), Some(0x10), "the MessageBox was recorded");
    assert!(s.planner.drain().is_some(), "the game's last clip drains");
    assert!(s.island_said.is_empty());
}

/// P1a: a big new window of the same process is adopted, which restarts the engine and
/// ends the drain the window's end started. Expected: the island says the clip is gone.
#[test]
fn an_adopted_window_ending_the_drain_says_last_clip_dropped() {
    let mut s = recording(UNITY);
    s.run_to(5_000);
    s.window_gone(0x10);
    s.focus(app_ev(UNITY, 10, 0x11, false));
    s.run_to(6_000);
    assert_eq!(engine_hwnd(&s), Some(0x11), "adopted");
    assert_eq!(s.island_said, vec![("Last clip dropped".to_string(), format!("{UNITY} started"))]);
}

/// The session-ends tick: a windowed Steam-library window already due when the game exits
/// (seen 1.6 s before, past WINDOWED_START) must wait for the drain, which the planner only
/// starts later that tick (`Transitions::live_drain`).
#[test]
fn a_window_due_on_the_exit_tick_waits_for_the_drain() {
    let mut s = recording(VAL);
    s.run_to(1_000);
    s.focus(steam_ev("GameLauncher", 52, 0x52));
    s.run_to(2_600);
    assert!(s.recording_of(10), "setup: the launcher has not replaced the live game");
    s.exit(10);
    s.run_to(8_000);
    assert!(s.recording_of(10) && s.planner.drain().is_some(), "the launcher ended the drain");
    assert_eq!(s.current().map(|t| t.pid), None);
}

/// P2: fullscreen Discord (a `detect.fullscreen_games` entry) during a drain waits like a
/// windowed Steam window, and commits once the drain is over.
#[test]
fn fullscreen_discord_waits_for_the_drain() {
    let mut s = draining();
    s.focus(app_ev("Discord", 30, 0x30, true));
    s.run_to(8_000);
    assert!(s.recording_of(10) && s.planner.drain().is_some(), "Discord ended the drain");
    let end = drain_end(&s);
    s.run_to(end + 1_000);
    assert!(s.recording_of(30), "Discord starts once the drain is over: {:?}", s.engine());
    assert!(s.island_said.is_empty(), "nothing was cut short");
}

/// P2: a long recording runs, the game exits, a windowed Steam window waits for the drain
/// and commits on the tick it runs out. Expected: the recording stopped because the game
/// closed (not "the recorded window changed").
#[test]
fn a_long_recording_ended_by_a_waiting_window_says_the_game_closed() {
    let mut s = recording(VAL);
    s.long_start();
    s.run_to(5_000);
    s.exit(10);
    s.run_to(5_100);
    s.focus(steam_ev("GameLauncher", 52, 0x52));
    let end = drain_end(&s);
    s.run_to(end + 200);
    assert!(s.sent_stop(), "the drain's end stops the long recording");
    assert_eq!(s.long_stopped(), Some(Trigger::AutoStopped { game_closed: true }));
}

/// P2: pinning a window during a drain ends it by the player's choice: no "Last clip
/// dropped" notice.
#[test]
fn a_pin_during_a_drain_is_not_a_dropped_clip() {
    let mut s = draining();
    s.pinned = Some(Target { hwnd: 0x77, pid: 77, name: "Pinned".into(), exe: "pinned.exe".into() });
    s.run_to(5_400);
    assert_eq!(engine_hwnd(&s), Some(0x77), "the pin is recorded");
    assert!(s.island_said.is_empty(), "notices: {:?}", s.island_said);
}
