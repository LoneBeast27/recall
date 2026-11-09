//! The transition matrix (rival-fixes lane 6, the maintainer 2026-11-06: "emphasize on behaviour of
//! cases like alt tabbing, transition of games etc"). One test per row of SPEC.md section 6,
//! driven through `transitions_sim::Sim` (the controller's pure pieces in pump order, with
//! injected time and window state). Each test states the expected behaviour first; the
//! asserts cover target, engine spawns, replay ring kept / wiped, drain, long recording and
//! island notices where the controller decides them.
//!
//! Not covered here (engine or OS side, no pure seam today): what WGC delivers for a
//! minimized window, Desktop Duplication occlusion / minimize holds (`recall/src/capture/
//! dda.rs` `run` FOLLOW_CHECK branch calls Win32 `still_covers`; lane fix/dda-occlusion adds
//! the `dda_logic` seam), and the overlay island hiding over exclusive fullscreen.

use crate::record_engine::EngineTarget;
use crate::record_notice::Situation;
use crate::record_target::{START_DEBOUNCE, TARGET_DEBOUNCE};
use crate::transitions_sim::{app_ev, game_ev, Sim};

const VAL: &str = "VALORANT-Win64-Shipping";
const APEX: &str = "r5apex_dx12";

/// Game A (pid 10) recording from t = 0, committed and applied by t = 400 ms.
fn recording_a() -> Sim {
    let mut s = Sim::new();
    s.focus(game_ev(VAL, 10, 0x10));
    s.run_to(400);
    assert!(s.recording_of(10), "setup: A is recorded");
    s
}

fn discord() -> crate::watcher::WatchEvent {
    app_ev("Discord", 30, 0x30, false) // windowed: not a target
}

fn desktop() -> crate::watcher::WatchEvent {
    app_ev("explorer", 4, 0x4, true)
}

/// Row 1: alt-tab out of a borderless game to Discord / the desktop and back (WGC).
/// Expected: same session, 0 engine restarts, ring kept, no notice (the island's own hide /
/// return over the game is the overlay's, not decided here).
#[test]
fn r01_alt_tab_out_of_a_borderless_game_and_back_keeps_the_session() {
    let mut s = recording_a();
    let mark = s.now;
    for (at, ev) in [(1_000, discord()), (6_000, desktop()), (20_000, game_ev(VAL, 10, 0x10))] {
        s.run_to(at);
        s.focus(ev);
    }
    s.run_to(25_000);
    assert_eq!(s.current().map(|t| t.pid), Some(10));
    assert!(s.recording_of(10) && s.ring_kept_since(mark), "0 restarts, ring kept");
    assert!(s.planner.drain().is_none());
    assert_eq!(s.notices_said, vec![Situation::Game(VAL.into())], "only the first pick-up was said");
}

/// Row 2: the same alt-tab with Desktop Duplication. Expected: frames held while occluded
/// (no Discord in the clip), 0 restarts, ring kept. Controller side asserted here (same as
/// row 1: nothing restarts the engine). The engine-side hold is lane fix/dda-occlusion's and
/// has no pure seam on master (see the module docs).
#[test]
fn r02_alt_tab_with_dda_controller_never_restarts_the_engine() {
    let mut s = recording_a();
    let mark = s.now;
    s.run_to(1_000);
    s.focus(app_ev("Discord", 30, 0x30, false));
    s.run_to(9_000);
    s.focus(game_ev(VAL, 10, 0x10));
    s.run_to(10_000);
    assert!(s.recording_of(10) && s.ring_kept_since(mark));
}

/// Rows 3 + 4: alt-tab out of an exclusive-fullscreen game (it minimizes), and Win+D /
/// minimize then restore. Expected: same session, 0 restarts, ring kept. The window stays
/// alive (IsWindow) and its process runs, so the tracker keeps it; while minimized and while
/// restoring (briefly not fullscreen = not a candidate) nothing ends the session.
/// Engine side (WGC frames of a minimized window, DDA "back on DDA after restore") is not
/// covered: on master a DDA follow-check failure restarts capture on WGC window capture and
/// nothing returns it to DDA (lane fix/dda-occlusion).
#[test]
fn r03_r04_minimize_and_restore_keeps_the_session() {
    let mut s = recording_a();
    let mark = s.now;
    s.run_to(1_000);
    s.focus(desktop()); // minimized: the desktop takes the foreground
    s.run_to(31_000); // longer than any debounce and the drain
    s.focus(app_ev(VAL, 10, 0x10, false)); // restoring: not fullscreen yet
    s.run_to(31_200);
    s.focus(game_ev(VAL, 10, 0x10)); // fullscreen again
    s.run_to(33_000);
    assert_eq!(s.current().map(|t| t.pid), Some(10));
    assert!(s.recording_of(10) && s.ring_kept_since(mark), "0 restarts, ring kept");
    assert_eq!(s.notices_said.len(), 1);
}

/// Row 5: alt-tab to a second game for < 2 s and back. Expected: no retarget.
#[test]
fn r05_a_second_game_under_two_seconds_never_retargets() {
    let mut s = recording_a();
    let mark = s.now;
    s.run_to(1_000);
    s.focus(game_ev(APEX, 20, 0x20));
    s.run_to(1_000 + TARGET_DEBOUNCE.as_millis() as u64 - 100);
    s.focus(game_ev(VAL, 10, 0x10));
    s.run_to(10_000);
    assert!(s.recording_of(10) && s.ring_kept_since(mark));
    assert_eq!(s.game_notices(APEX), 0);
}

/// Row 6a: alt-tab to a second game for > 2 s. Expected: retarget to B at 2 s (A's ring
/// ends: one spawn), one "Recording B" notice.
#[test]
fn r06a_a_second_game_over_two_seconds_retargets() {
    let mut s = recording_a();
    let mark = s.now;
    s.run_to(1_000);
    s.focus(game_ev(APEX, 20, 0x20));
    s.run_to(5_000);
    assert!(s.recording_of(20));
    let at = s.applied_at(20).unwrap();
    assert!(at >= s.ms(1_000) + TARGET_DEBOUNCE && at <= s.ms(1_000) + TARGET_DEBOUNCE + crate::transitions_sim::TICK);
    assert_eq!(s.spawns_since(mark), 1);
    assert_eq!(s.game_notices(APEX), 1);
}

/// Row 6b + 6c: a save pressed in the switch window (before B commits) goes to A's engine,
/// and the retarget is held while it is in flight, then applies once it lands.
#[test]
fn r06b_a_save_in_the_switch_window_lands_in_a_and_holds_the_retarget() {
    let mut s = recording_a();
    s.run_to(1_000);
    s.focus(game_ev(APEX, 20, 0x20));
    s.run_to(2_500);
    assert!(s.save());
    assert!(matches!(&s.sent[0].1, EngineTarget::Window(t) if t.pid == 10), "A's clip");
    s.run_to(4_000);
    assert_eq!(s.current().map(|t| t.pid), Some(20), "the tracker moved on");
    assert!(s.recording_of(10) && s.held_ticks > 0, "held while the save is in flight");
    s.saved();
    s.run_to(4_100);
    assert!(s.recording_of(20), "applies on the first idle tick");
}

/// Row 6d (lane fix/longrec-game-exit): alt-tab to game B for > 2 s while the long recording
/// runs. Expected: the long recording is stopped (record_stop) and B is recorded; never held
/// indefinitely. Master: held until Alt+L.
#[test]
fn r06d_a_second_game_during_the_long_recording_stops_it_then_retargets() {
    let mut s = recording_a();
    s.long_start();
    s.run_to(1_000);
    s.focus(game_ev(APEX, 20, 0x20));
    s.run_to(60_000);
    assert!(s.sent_stop(), "record_stop sent");
    assert!(s.recording_of(20), "B recorded");
}

/// Row 7: quit game A, launch game B within the drain. Expected: A's drain runs (save
/// works, at_last_frame), B commits after START_DEBOUNCE and is applied the same tick (well
/// within 300 ms of committing), ending the drain; a save pressed before B commits is A's.
#[test]
fn r07_quit_a_and_launch_b_within_the_drain() {
    let mut s = recording_a();
    s.run_to(10_000);
    s.exit(10);
    s.run_to(10_100);
    assert!(s.planner.drain().is_some(), "A drains");
    assert!(s.notices_said.contains(&Situation::Draining { secs: 120 }));
    s.run_to(30_000);
    s.focus(game_ev(APEX, 20, 0x20));
    s.run_to(30_150);
    assert!(s.save(), "a save during the drain is accepted");
    assert!(matches!(&s.sent[0].1, EngineTarget::Window(t) if t.pid == 10), "A's clip");
    s.saved();
    s.run_to(31_000);
    let at = s.applied_at(20).expect("B recorded");
    assert!(at <= s.ms(30_000) + START_DEBOUNCE + crate::transitions_sim::TICK, "B within the start debounce");
    assert!(s.planner.drain().is_none(), "B ended A's drain");
    assert_eq!(s.game_notices(APEX), 1);
}

/// Row 8: quit game A into its launcher (a denied stem). Expected: drain, then waiting (the
/// end is silent); the launcher is never a target.
#[test]
fn r08_quit_into_the_launcher_drains_then_waits() {
    let mut s = recording_a();
    s.run_to(10_000);
    s.focus(app_ev("RiotClientUx", 40, 0x40, true));
    s.exit(10);
    s.run_to(10_100);
    assert!(s.planner.drain().is_some());
    s.run_to(10_000 + 121_000);
    assert_eq!(*s.engine(), EngineTarget::Off, "drain over: waiting");
    assert!(s.current().is_none() && s.tracker.pending().is_none(), "the launcher is never a target");
    let said: Vec<_> = s.notices_said.iter().map(|n| n.title()).collect();
    assert_eq!(said, vec!["Recording", "Game closed"], "the drain's end is silent");
}

/// Row 9: the game restarts itself (same exe, new pid and window: a VALORANT client
/// restart). Expected: the new instance commits after START_DEBOUNCE (not 2 s), one
/// retarget, and no notice is repeated (one "Game closed", one "Recording" for the new pid;
/// my reading: silence across the restart would need the "Game closed" notice delayed, a
/// design call left out of this lane).
#[test]
fn r09_a_game_restarting_itself_retargets_after_the_start_debounce() {
    let mut s = recording_a();
    let mark = s.now;
    s.run_to(10_000);
    s.window_gone(0x10);
    s.exit(10);
    s.run_to(12_000);
    s.focus(game_ev(VAL, 11, 0x11));
    s.run_to(14_000);
    let at = s.applied_at(11).expect("the new instance is recorded");
    assert!(at <= s.ms(12_000) + START_DEBOUNCE + crate::transitions_sim::TICK);
    assert_eq!(s.spawns_since(mark), 1);
    assert_eq!(s.game_notices(VAL), 2, "the first pick-up and the restart, never more");
    assert_eq!(s.notices_said.iter().filter(|n| matches!(n, Situation::Draining { .. })).count(), 1);
}

/// Row 10: a fullscreen <-> borderless <-> windowed switch that makes a new window of the
/// same process. Expected: follows within START_DEBOUNCE (one engine spawn; the ring is
/// wiped there, because the window handle is a spawn-time engine setting,
/// RECALL_CAPTURE_HWND). A switch that keeps the window but changes its size wipes the ring
/// in the engine instead (`encoder.rs`: "capture resolution changed; reopening NVENC").
/// Keeping it would need a live window switch in the engine and an encoder that scales.
#[test]
fn r10_a_mode_switch_window_follows_within_the_start_debounce() {
    let mut s = recording_a();
    let mark = s.now;
    s.run_to(5_000);
    s.focus(game_ev(VAL, 10, 0x11)); // new window, the old one still alive
    s.run_to(5_000 + START_DEBOUNCE.as_millis() as u64 + 50);
    assert!(matches!(s.engine(), EngineTarget::Window(t) if t.hwnd == 0x11));
    assert_eq!(s.spawns_since(mark), 1, "one spawn: the ring restarts here");
    // Windowed (same window, not fullscreen): not a candidate, the session stays.
    s.focus(app_ev(VAL, 10, 0x11, false));
    s.run_to(15_000);
    assert!(matches!(s.engine(), EngineTarget::Window(t) if t.hwnd == 0x11));
    assert_eq!(s.spawns_since(mark), 1);
    assert_eq!(s.game_notices(VAL), 1, "a same-game window change is silent");
}

/// Row 11: two games open, focus flipping every 1 s. Expected: TARGET_DEBOUNCE holds, no
/// retarget storm (<= 1 spawn: the first game's).
#[test]
fn r11_focus_flipping_between_two_games_never_storms() {
    let mut s = Sim::new();
    for i in 0..30u64 {
        let ev = if i % 2 == 0 { game_ev(VAL, 10, 0x10) } else { game_ev(APEX, 20, 0x20) };
        s.run_to(i * 1_000);
        s.focus(ev);
    }
    s.run_to(30_500);
    assert_eq!(s.spawns_since(s.t0 - std::time::Duration::from_millis(1)), 1);
    assert!(s.recording_of(10));
}

/// Row 12a: the game crashes during a save. Expected: the save completes on the kept
/// (draining) engine; the drain then ends normally.
#[test]
fn r12a_a_crash_during_a_save_lets_the_save_finish() {
    let mut s = recording_a();
    s.run_to(5_000);
    assert!(s.save());
    s.exit(10);
    s.run_to(6_000);
    assert!(s.recording_of(10) && s.planner.drain().is_some(), "engine kept");
    s.saved();
    s.run_to(5_000 + 121_000);
    assert_eq!(*s.engine(), EngineTarget::Off, "drain ends");
    assert_eq!(s.held_ticks, 0);
}

/// Row 12b (lane fix/longrec-game-exit): the game crashes during the long recording.
/// Expected: at the drain's end record_stop is sent and the engine stops (no file growing
/// with audio and no video). Master: held forever.
#[test]
fn r12b_a_crash_during_the_long_recording_stops_it_at_the_drain_end() {
    let mut s = recording_a();
    s.long_start();
    s.run_to(5_000);
    s.exit(10);
    s.run_to(5_000 + 125_000);
    assert!(s.sent_stop(), "record_stop sent at the drain's end");
    s.activity.on_event(&serde_json::json!({"type": "recording_stopped", "path": "r.mp4"}), 0.0);
    s.run_to(5_000 + 126_000);
    assert_eq!(*s.engine(), EngineTarget::Off);
}

/// Row 12c (lane fix/longrec-game-exit): quit A during the long recording and launch B.
/// Expected: the long recording stops at once and B is recorded.
#[test]
fn r12c_a_new_game_after_a_crash_during_the_long_recording_is_recorded() {
    let mut s = recording_a();
    s.long_start();
    s.run_to(5_000);
    s.exit(10);
    s.run_to(8_000);
    s.focus(game_ev(APEX, 20, 0x20));
    s.run_to(20_000);
    assert!(s.sent_stop());
    assert!(s.recording_of(20));
}

/// Row 13: pause during a drain, then resume. Expected: the drain ends cleanly (engine
/// stopped, no drain, "Recording paused"), and resume picks the current foreground game at
/// once (it committed while paused: the tracker keeps following).
#[test]
fn r13_pause_during_a_drain_then_resume_picks_the_current_game_at_once() {
    let mut s = recording_a();
    s.run_to(5_000);
    s.exit(10);
    s.run_to(6_000);
    assert!(s.planner.drain().is_some());
    s.pause();
    s.run_to(6_100);
    assert_eq!(*s.engine(), EngineTarget::Off);
    assert!(s.planner.drain().is_none(), "the drain ended with the pause");
    assert_eq!(s.notices_said.last(), Some(&Situation::Paused));
    s.focus(game_ev(APEX, 20, 0x20));
    s.run_to(10_000);
    assert_eq!(*s.engine(), EngineTarget::Off, "nothing records while paused");
    let resumed = s.now;
    s.resume();
    assert!(s.recording_of(20), "resume applies the tracked game the same tick");
    assert_eq!(s.applied_at(20), Some(resumed));
    assert_eq!(s.notices_said.last(), Some(&Situation::Game(APEX.into())));
}
