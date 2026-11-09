//! Round-8 rows for fix/deep-detect-p9 (2026-11-07): a session whose only proof is
//! `detect.fullscreen_games` lives only while its window is fullscreen (R8-detect-1), a
//! registration change that ends a session stops capturing it at once and drops its ring
//! (R8-detect-2, R8-detect-5), and the harness applies the proof rule (R8-detect-6). Same
//! harness as `transitions_tests`.

use crate::config::{Config, Override};
use crate::record_engine::EngineTarget;
use crate::transitions_sim::{app_ev, game_ev, steam_ev, Sim};
use crate::types::RuleKind;

fn with(exe: &str, kind: RuleKind) -> Config {
    let mut cfg = Config::default();
    cfg.overrides.push(Override { exe: exe.into(), kind, clamp: None });
    cfg
}

fn engine_hwnd(s: &Sim) -> Option<isize> {
    match s.engine() {
        EngineTarget::Window(t) => Some(t.hwnd),
        _ => None,
    }
}

/// Fullscreen Discord (pid 30) recorded from t = 0 through the default `fullscreen_games`.
fn recording_discord() -> Sim {
    let mut s = Sim::new();
    s.focus(app_ev("Discord", 30, 0x30, true));
    s.run_to(400);
    assert!(s.recording_of(30), "setup: fullscreen Discord is recorded");
    s
}

#[test]
fn fullscreen_discord_pressing_esc_ends_the_session_and_records_nothing_after() {
    let mut s = recording_discord();
    s.run_to(5_000);
    let mark = s.ms(4_999); // the drop applies at this very tick
    s.seen(app_ev("Discord", 30, 0x30, false)); // Esc: same window, windowed (the 1 s probe)
    s.run_to(5_050);
    assert!(s.current().is_none(), "the session ended");
    assert!(!s.recording_of(30), "the engine left Discord at once");
    assert!(s.planner.drain().is_none(), "no drain linger on the windowed window");
    assert!(!s.ring_kept_since(mark), "the ring was dropped");
    s.run_to(30_000);
    assert!(!s.recording_of(30), "nothing recorded after");
}

#[test]
fn alt_tab_away_from_fullscreen_discord_keeps_it_like_a_game() {
    let mut s = recording_discord();
    let mark = s.now;
    s.run_to(1_000);
    s.focus(app_ev("notepad", 40, 0x40, false));
    s.run_to(10_000);
    assert!(s.recording_of(30) && s.ring_kept_since(mark), "kept while it stays fullscreen");
}

#[test]
fn a_remade_windowed_discord_window_is_not_adopted() {
    let mut s = recording_discord();
    s.run_to(1_000);
    s.window_gone(0x30);
    s.focus(app_ev("Discord", 30, 0x31, false));
    s.run_to(5_000);
    assert!(s.current().is_none(), "not adopted on fullscreen_games alone");
    assert_ne!(engine_hwnd(&s), Some(0x31));
}

/// An override exe (pid 50) recorded windowed from t = 0.
fn recording_override(stem: &str, fullscreen: bool) -> Sim {
    let mut s = Sim::new();
    s.set_config(with(stem, RuleKind::SdrGame));
    s.focus(app_ev(stem, 50, 0x50, fullscreen));
    s.run_to(400);
    assert!(s.recording_of(50), "setup: the override is recorded");
    s
}

#[test]
fn never_record_on_the_recorded_app_stops_capture_now_and_drops_the_ring() {
    for probe_fails in [false, true] {
        let mut s = recording_override("MysteryApp", false);
        s.probe_fails = probe_fails;
        s.run_to(5_000);
        let mark = s.ms(4_999); // the drop applies at this very tick
        s.set_config(with("mysteryapp.exe", RuleKind::None));
        assert!(s.current().is_none(), "the session ended (probe fails: {probe_fails})");
        s.run_to(5_050);
        assert!(!s.recording_of(50) && s.planner.drain().is_none(), "no linger (probe fails: {probe_fails})");
        assert!(!s.ring_kept_since(mark));
        s.run_to(20_000);
        assert!(!s.recording_of(50));
    }
}

#[test]
fn removing_the_override_of_an_unproven_app_stops_capture_now() {
    for probe_fails in [false, true] {
        let mut s = recording_override("MysteryApp", false);
        s.probe_fails = probe_fails;
        s.run_to(5_000);
        s.set_config(Config::default());
        s.run_to(5_050);
        assert!(!s.recording_of(50) && s.planner.drain().is_none(), "probe fails: {probe_fails}");
    }
}

#[test]
fn removing_an_override_rechecks_the_window_at_once_under_the_proof_rule_r8_detect_5() {
    // A plain fullscreen app: without the override it needs a render verdict, so the session
    // ends (ring dropped) and the same window is fed again at once: its verdict records it.
    let mut s = recording_override("MysteryGame", true);
    s.run_to(5_000);
    let mark = s.now;
    s.set_config(Config::default());
    s.run_to(5_050);
    assert!(!s.recording_of(50), "ended under the proof rule");
    s.seen(app_ev("MysteryGame", 50, 0x50, true)); // the same window, unchanged
    s.run_to(6_000);
    assert!(s.recording_of(50), "fed again: the render verdict records it");
    assert_eq!(s.spawns_since(mark), 1, "one new engine for the re-detected window");
}

#[test]
fn a_steam_game_keeps_its_session_when_its_override_goes() {
    let mut s = Sim::new();
    s.set_config(with("SomeGame", RuleKind::SdrGame));
    s.focus(steam_ev("SomeGame", 50, 0x50));
    s.run_to(2_000);
    assert!(s.recording_of(50));
    let mark = s.now;
    s.set_config(Config::default());
    s.run_to(3_000);
    assert!(s.recording_of(50) && s.ring_kept_since(mark), "Steam still proves it");
}

#[test]
fn the_matrix_applies_the_proof_rule_r8_detect_6() {
    let mut s = Sim::new();
    s.can_check = false;
    s.focus(game_ev("MysteryGame", 60, 0x60));
    s.run_to(3_000);
    assert!(!s.recording_of(60), "no proof and no verdict: not recorded");
    s.focus(game_ev("VALORANT-Win64-Shipping", 10, 0x10));
    s.run_to(4_000);
    assert!(s.recording_of(10), "a known game needs no check");
    let mut s = Sim::new();
    s.not_games = vec!["MysteryTool".into()];
    s.focus(game_ev("MysteryTool", 61, 0x61));
    s.run_to(3_000);
    assert!(!s.recording_of(61), "a 'not a game' render verdict");
    s.focus(game_ev("MysteryGame", 62, 0x62));
    s.run_to(4_000);
    assert!(s.recording_of(62), "a 'game' render verdict");
}
