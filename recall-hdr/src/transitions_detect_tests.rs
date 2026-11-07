//! Round-2 rows for fix/deep-detect-p2 (2026-11-07): a known game's small dialog (an
//! out-of-video-memory box, a crash box) never takes the session (R2-4), and a game that
//! goes fullscreen in place (Alt+Enter, a borderless toggle) is protected from a windowed
//! Steam tool from then on (R2-3). Same harness as `transitions_gate_tests`.

use crate::record_engine::EngineTarget;
use crate::transitions_sim::{game_ev, steam_ev, Sim};
use crate::watcher::WatchEvent;

const VAL: &str = "VALORANT-Win64-Shipping";

fn engine_hwnd(s: &Sim) -> Option<isize> {
    match s.engine() {
        EngineTarget::Window(t) => Some(t.hwnd),
        _ => None,
    }
}

/// A windowed top-level window of the known game's own process.
fn valorant_box(hwnd: isize) -> WatchEvent {
    WatchEvent { fullscreen: false, fullscreen_on_target: false, title: "VALORANT".into(), ..game_ev(VAL, 10, hwnd) }
}

#[test]
fn a_known_games_message_box_never_retargets() {
    let mut s = Sim::new();
    s.focus(game_ev(VAL, 10, 0x10));
    s.run_to(400);
    assert_eq!(engine_hwnd(&s), Some(0x10), "setup: the game is recorded");
    let mark = s.now;
    s.client = (400, 150);
    s.focus(valorant_box(0x11));
    s.run_to(4_000);
    assert_eq!(engine_hwnd(&s), Some(0x10), "the dialog never takes the session");
    assert!(s.ring_kept_since(mark), "the replay ring is kept");
    // After a crash: the main window is gone, the error box is left.
    s.window_gone(0x10);
    s.focus(valorant_box(0x11));
    s.run_to(8_000);
    assert_ne!(engine_hwnd(&s), Some(0x11), "the crash box is not recorded");
    assert!(!s.island_said.iter().any(|(t, _)| t == "Last clip dropped"), "the drain is not cut: {:?}", s.island_said);
}

#[test]
fn a_windowed_known_game_of_full_size_still_starts_on_300_ms() {
    let mut s = Sim::new();
    s.client = (1280, 720);
    s.focus(valorant_box(0x10));
    s.run_to(400);
    assert_eq!(engine_hwnd(&s), Some(0x10), "G10 stands for a real game window");
}

#[test]
fn an_in_place_fullscreen_flip_protects_the_session() {
    // A Steam game starts windowed, then goes fullscreen in the same window (Alt+Enter).
    let mut s = Sim::new();
    let game = steam_ev("SteamGame", 1, 0x10);
    s.focus(game.clone());
    s.run_to(1_600);
    assert_eq!(engine_hwnd(&s), Some(0x10), "setup: the windowed game is recorded");
    s.run_to(2_000);
    s.seen(game.clone());
    s.seen(WatchEvent { fullscreen: true, fullscreen_on_target: true, ..game });
    let mark = s.now;
    // Blender from a Steam library, windowed, in front for 5 s.
    s.run_to(3_000);
    s.focus(steam_ev("blender", 2, 0x20));
    s.run_to(8_000);
    assert_eq!(engine_hwnd(&s), Some(0x10), "the fullscreen game keeps the session");
    assert!(s.ring_kept_since(mark), "its ring is kept");
}
