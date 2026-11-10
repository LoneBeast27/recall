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
    assert!(s.ring_dropped_since(mark), "the ring was dropped");
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
        assert!(s.ring_dropped_since(mark));
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

/// Final review F-detect-1: Esc while a save is written must not stop the engine under it (the
/// old engine's quit gives a save 3 s, then aborts it).
#[test]
fn esc_while_a_save_is_written_does_not_stop_the_engine_under_it() {
    let mut s = recording_discord();
    s.run_to(5_000);
    assert!(s.save());
    let mark = s.ms(4_999); // a stop applies at this very tick
    s.seen(app_ev("Discord", 30, 0x30, false)); // Esc during the save
    s.run_to(5_050);
    assert!(s.ring_kept_since(mark), "engine stopped under an in-flight save");
    s.saved();
    s.run_to(5_200);
    assert!(!s.recording_of(30), "then it leaves Discord");
}

/// F-detect-1 (option b): the engine is released, not quit: capture stops now, the save in
/// flight finishes, and Discord is no longer counted as recorded.
#[test]
fn esc_during_a_save_releases_the_engine_and_takes_no_new_save() {
    let mut s = recording_discord();
    s.run_to(5_000);
    assert!(s.save());
    let mark = s.ms(4_999);
    s.seen(app_ev("Discord", 30, 0x30, false));
    s.run_to(5_050);
    assert_eq!(s.released.len(), 1, "one stop_capture");
    assert_eq!(s.released[0].1, EngineTarget::Off, "then no engine: nothing else is wanted");
    assert!(s.ring_kept_since(mark), "no quit under the save");
    assert!(!s.recording_of(30) && s.planner.drain().is_none(), "not counted as recording Discord, no drain");
    assert!(!s.save(), "no new save from the released ring");
    s.saved();
    s.run_to(30_000);
    assert_eq!(s.spawns_since(mark), 0, "nothing respawned for Discord");
}

/// F-detect-1 fallback (option a): an engine without `stop_capture` keeps the stop until the
/// save lands, with no drain linger, then stops.
#[test]
fn an_older_engine_holds_the_privacy_stop_until_the_save_lands() {
    let mut s = recording_discord();
    s.engine_releases = false;
    s.run_to(5_000);
    assert!(s.save());
    let mark = s.ms(4_999);
    s.seen(app_ev("Discord", 30, 0x30, false));
    s.run_to(6_000);
    assert!(s.ring_kept_since(mark) && s.held_ticks > 0, "held for the save");
    s.saved();
    s.run_to(6_100);
    assert!(!s.recording_of(30) && s.ring_dropped_since(mark), "stopped once the save landed, no 120 s drain");
    assert!(s.released.is_empty());
}

/// Never record during a save: released too (R8-detect-2's path, end_for_registration).
#[test]
fn never_record_during_a_save_releases_the_engine() {
    let mut s = recording_override("MysteryApp", false);
    s.run_to(5_000);
    assert!(s.save());
    let mark = s.ms(4_999);
    s.set_config(with("mysteryapp.exe", RuleKind::None));
    s.run_to(5_050);
    assert!(s.ring_kept_since(mark) && s.released.len() == 1 && !s.recording_of(50));
}

/// R2-detect-r10-1 (a): with no save in flight, a privacy stop is the plain quit (8 s), never a
/// release (a released engine may hold the slot far longer).
#[test]
fn a_privacy_stop_with_no_save_quits_the_engine_and_releases_nothing() {
    let mut s = recording_discord();
    s.run_to(5_000);
    let mark = s.ms(4_999);
    s.seen(app_ev("Discord", 30, 0x30, false));
    s.run_to(5_050);
    assert!(s.released.is_empty(), "no stop_capture without a save");
    assert!(!s.ring_kept_since(mark) && !s.recording_of(30), "stopped at once, as before");
}

/// R2-detect-r10-2: while an older engine's privacy stop is held for a save, no new save is
/// taken (it would save the private ring and extend the hold).
#[test]
fn an_older_engine_takes_no_save_while_its_privacy_stop_is_held() {
    let mut s = recording_discord();
    s.engine_releases = false;
    s.run_to(5_000);
    assert!(s.save());
    s.seen(app_ev("Discord", 30, 0x30, false));
    s.run_to(5_500);
    assert!(s.recording_of(30), "setup: held for the save");
    assert!(!s.save(), "no save of the private ring during the hold");
    s.saved();
    s.run_to(5_600);
    assert!(!s.recording_of(30), "the hold ends with the first save");
}

/// R2-detect-r10-p2-1 end to end: Esc during a long save releases the engine; while its
/// drain reports progress (`save_progress`) for well past 120 s, and past the 10 min cap,
/// the release never settles (the engine is not cut to 8 s). A stall of 120 s settles it, and
/// so does the save's answer.
#[test]
fn a_released_save_that_keeps_moving_is_never_settled_a_stalled_one_is() {
    use std::time::Duration;
    let progress = |s: &mut Sim, step: u64| {
        let ev = serde_json::json!({"type": "save_progress", "save_writing": {"bytes": step, "queued": 0, "step": step}});
        let now = s.awake();
        s.activity.on_engine_event(&ev, 0.0, now);
    };
    let mut s = recording_discord();
    s.engine_heartbeats = false; // an older engine: the controller judges the stall (P8)
    s.run_to(5_000);
    assert!(s.save());
    s.seen(app_ev("Discord", 30, 0x30, false));
    s.run_to(5_050);
    assert_eq!(s.released.len(), 1, "released for the save");
    for k in 1..=180u64 {
        s.now += Duration::from_secs(5); // 15 min of a save still writing
        progress(&mut s, k);
        let now = s.awake();
        assert!(!s.activity.release_may_settle(now), "settled at {} s while the save moved", k * 5);
    }
    let now = s.awake() + Duration::from_secs(134);
    assert!(!s.activity.release_may_settle(now), "a 134 s pause is no stall");
    let now = s.awake() + Duration::from_secs(135);
    assert!(s.activity.release_may_settle(now), "135 s with no progress: the engine's 120 s rule + margin");
    s.saved();
    let idle = serde_json::json!({"type": "save_progress", "stalled_for": 0}); // the saver is empty now
    let now = s.awake();
    s.activity.on_engine_event(&idle, 0.0, now);
    assert!(s.activity.release_may_settle(now), "answered (and the saver empty): settles at once");
}

/// R2-detect-r10-p2-2: the old-engine refusal reads like the other refusals (under "Save failed").
#[test]
fn the_privacy_hold_refusal_names_the_app_only() {
    let mut s = recording_discord();
    s.engine_releases = false;
    s.run_to(5_000);
    assert!(s.save());
    s.seen(app_ev("Discord", 30, 0x30, false));
    s.run_to(5_500);
    assert_eq!(s.planner.privacy_refusal().as_deref(), Some("Discord.exe is no longer recorded"));
}

/// Gate p5-1: a save that has written past its cap (>= 10 min) is still in flight: a privacy
/// stop releases the engine and waits for it, never the plain quit that cuts it.
#[test]
fn a_privacy_stop_waits_for_a_save_already_past_its_cap() {
    use std::time::Duration;
    let mut s = recording_discord();
    s.run_to(5_000);
    assert!(s.save());
    for k in 1..=132u64 {
        s.now += Duration::from_secs(5); // 11 min of a save still writing (the engine's status)
        let ev = serde_json::json!({"type": "status", "save_writing": {"bytes": k, "queued": 0, "step": k}});
        let now = s.awake();
        s.activity.on_engine_event(&ev, 0.0, now);
    }
    s.seen(app_ev("Discord", 30, 0x30, false));
    let mark = s.now - Duration::from_millis(1);
    s.tick();
    assert_eq!(s.released.len(), 1, "released for the save past its cap");
    assert!(s.ring_kept_since(mark), "no quit under it");
}
