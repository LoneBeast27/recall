//! Transition rows added by the coordinator's second-pass audit (ADDENDUM-2026-11-06, lane 6:
//! T2, T3, T5, T9, T11; T1 is engine-side, `recall/src/audio_sleep_tests.rs`). Expected
//! behaviour first, measured on master; a failing row is fixed in another lane set
//! (`transitions-fixes`, branch fix/transition-targets) and marked ignored. Same harness as
//! `transitions_tests`.

use crate::record_engine::EngineTarget;
use crate::transitions_sim::{app_ev, game_ev, steam_ev, Sim};

const VAL: &str = "VALORANT-Win64-Shipping";
const APEX: &str = "r5apex_dx12";

fn recording_a() -> Sim {
    let mut s = Sim::new();
    s.focus(game_ev(VAL, 10, 0x10));
    s.run_to(400);
    assert!(s.recording_of(10), "setup: A is recorded");
    s
}

/// A's drain running from t = 5 s.
fn draining_a() -> Sim {
    let mut s = recording_a();
    s.run_to(5_000);
    s.exit(10);
    s.run_to(5_100);
    assert!(s.planner.drain().is_some(), "setup: A drains");
    s
}

/// T2a: a crash handler shipped in the game's Steam folder (UnityCrashHandler64,
/// CrashReportClient, crashpad_handler) opening during the drain. Expected: the drain goes
/// on (A's last clip stays saveable), the handler is never recorded.
/// Master: a Steam-library exe is a candidate with no render check, so it commits after
/// START_DEBOUNCE and ends the drain.
#[test]
#[ignore = "fixed in transitions-fixes/fix/transition-targets"]
fn t2a_a_crash_handler_during_the_drain_does_not_end_it() {
    for stem in ["UnityCrashHandler64", "CrashReportClient", "crashpad_handler"] {
        let mut s = draining_a();
        s.focus(steam_ev(stem, 50, 0x50));
        s.run_to(8_000);
        assert!(s.recording_of(10) && s.planner.drain().is_some(), "{stem} ended the drain");
    }
}

/// T2b: WerFault (not in a Steam library, windowed) during the drain: not a candidate.
#[test]
fn t2b_werfault_during_the_drain_does_not_end_it() {
    let mut s = draining_a();
    s.focus(app_ev("WerFault", 51, 0x51, false));
    s.run_to(8_000);
    assert!(s.recording_of(10) && s.planner.drain().is_some());
}

/// T2c: a windowed Steam-library launcher (the game's own launcher exe) opening during the
/// drain. Expected: the drain goes on. Master: commits like a windowed Steam game.
#[test]
#[ignore = "fixed in transitions-fixes/fix/transition-targets"]
fn t2c_a_windowed_steam_launcher_during_the_drain_does_not_end_it() {
    let mut s = draining_a();
    s.focus(steam_ev("GameLauncher", 52, 0x52));
    s.run_to(8_000);
    assert!(s.recording_of(10) && s.planner.drain().is_some());
}

/// T3: Discord fullscreen (a `detect.fullscreen_games` entry by default) in front of a live
/// game for 10 s. Expected: the game stays the target. Master: Discord replaces it after
/// TARGET_DEBOUNCE.
#[test]
#[ignore = "fixed in transitions-fixes/fix/transition-targets"]
fn t3_fullscreen_discord_over_a_live_game_does_not_replace_it() {
    let mut s = recording_a();
    let mark = s.now;
    s.run_to(1_000);
    s.focus(app_ev("Discord", 30, 0x30, true));
    s.run_to(11_000);
    s.focus(game_ev(VAL, 10, 0x10));
    s.run_to(12_000);
    assert!(s.recording_of(10) && s.ring_kept_since(mark), "Discord took the target");
}

/// T5: the game recreates its window (same pid, new hwnd) windowed, outside a Steam library,
/// and the old window is destroyed. Expected: the game keeps being recorded (no drain,
/// no "Game closed"). Master: the new window is not a candidate (not fullscreen), the old
/// window's end ends the session: drain, then waiting.
#[test]
#[ignore = "fixed in transitions-fixes/fix/transition-targets"]
fn t5_a_windowed_new_window_of_the_game_keeps_it_recorded() {
    let mut s = recording_a();
    s.run_to(5_000);
    s.window_gone(0x10);
    s.focus(app_ev(VAL, 10, 0x11, false));
    s.run_to(10_000);
    assert!(s.recording_of(10), "engine: {:?}", s.engine());
    assert!(s.planner.drain().is_none(), "no drain for a live game");
}

/// T9: two games swapped every 2.5 s (each holds past TARGET_DEBOUNCE). Expected: restarts
/// are rate-limited, at most one retarget per 10 s (<= 4 spawns in 30 s, the first
/// included). Master: one restart per swap.
#[test]
#[ignore = "fixed in transitions-fixes/fix/transition-targets"]
fn t9_games_swapped_every_two_and_a_half_seconds_are_rate_limited() {
    let mut s = Sim::new();
    for i in 0..12u64 {
        let ev = if i % 2 == 0 { game_ev(VAL, 10, 0x10) } else { game_ev(APEX, 20, 0x20) };
        s.run_to(i * 2_500);
        s.focus(ev);
    }
    s.run_to(30_000);
    let spawns = s.applied.iter().filter(|(_, t)| *t != EngineTarget::Off).count();
    assert!(spawns <= 4, "{spawns} engine spawns in 30 s");
}

/// T11: a game B pending against live A, then a foreground the watcher cannot identify
/// (OpenProcess fails, e.g. an elevated or protected window). Expected: B's pending switch
/// is cancelled (the player left B). Master: no event reaches the tracker, so B commits.
#[test]
#[ignore = "fixed in transitions-fixes/fix/transition-targets"]
fn t11_an_unidentifiable_foreground_cancels_a_pending_game() {
    let mut s = recording_a();
    s.run_to(1_000);
    s.focus(game_ev(APEX, 20, 0x20));
    s.run_to(1_500);
    s.focus_unidentified();
    s.run_to(6_000);
    assert!(s.recording_of(10), "B committed although it lost the foreground");
}
