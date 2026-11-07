//! Engine restarts while a closed game drains (AUDIT-3 U2, fix/settings-drain-v2). Every
//! restart that is no game switch goes through `record_restart` (the controller's
//! `request_engine_restart` / `restart_engine` / `activity_tick`): a settings or resume
//! restart waits for the drain, a recovery restart never does. Same harness as
//! `transitions_tests`.

use std::time::Duration;

use crate::record_engine::EngineTarget;
use crate::record_recover::{EncoderRecovery, Recovery};
use crate::record_restart::{RestartReason, Restarted};
use crate::supervisor::EngineHealth;
use crate::transitions_sim::{game_ev, Sim};

const VAL: &str = "VALORANT-Win64-Shipping";
const APEX: &str = "r5apex_dx12";

fn settings_notice() -> (String, String) {
    ("Settings applied".to_string(), "The replay buffer restarted".to_string())
}

/// A recorded from 0.4 s; it exits at 5 s and drains (120 s buffered: until 125 s).
fn draining_a(desktop_when_idle: bool) -> Sim {
    let mut s = Sim::new();
    s.set_desktop_when_idle(desktop_when_idle);
    s.focus(game_ev(VAL, 10, 0x10));
    s.run_to(5_000);
    assert!(s.recording_of(10), "setup: A is recorded");
    s.exit(10);
    s.run_to(5_100);
    assert!(s.planner.drain().is_some(), "setup: A drains");
    s
}

/// The planned restarts: a settings change and a resume from sleep.
type Kind = (&'static str, fn(&mut Sim));

fn resume(s: &mut Sim) {
    s.restart(RestartReason::Resume);
}

const KINDS: [Kind; 2] = [("settings", Sim::settings_changed), ("resume", resume)];

/// A's last clip is still saved from A's engine (the drain was not cut).
fn save_last_clip(s: &mut Sim, kind: &str) {
    assert!(s.save(), "{kind}: A's last clip is still saveable");
    let (cmd, target) = s.sent.last().expect("save sent");
    assert_eq!(cmd, "save_clip");
    assert!(matches!(target, EngineTarget::Window(t) if t.pid == 10), "{kind}: saved from A's engine");
    s.saved();
}

/// U2a: a settings change (buffer length, codec, folder...) while the island shows "Last
/// clip kept", and a resume from sleep. The drain goes on, the restart waits; a drain that
/// ends in `Off` needs no restart (the next start reads the new settings).
/// Before: the restart applied `wanted()` = Off at once and the last clip was gone.
#[test]
fn u2a_a_planned_restart_during_the_drain_keeps_the_last_clip() {
    for (kind, restart) in KINDS {
        let mut s = draining_a(false);
        let mark = s.now;
        restart(&mut s);
        assert!(s.ring_kept_since(mark) && s.planner.drain().is_some(), "{kind}: the drain goes on");
        assert!(s.pending_restart.is_some(), "{kind}: the restart waits for the drain");
        s.run_to(60_000);
        save_last_clip(&mut s, kind);
        s.run_to(126_000);
        assert_eq!(*s.engine(), EngineTarget::Off, "{kind}: drain over: waiting");
        assert_eq!(s.pending_restart, None, "{kind}: a drain that ends in Off needs no restart");
        assert_eq!(s.spawns_since(mark), 0, "{kind}: no respawn after the drain");
        assert!(s.island_said.is_empty(), "{kind}: no game buffer was wiped by a restart");
    }
}

/// U2b: the same change mid-game: one restart, and the island says why the buffer is empty.
#[test]
fn u2b_a_mid_game_settings_change_restarts_with_a_notice() {
    let mut s = draining_a(false);
    s.focus(game_ev(APEX, 20, 0x20));
    s.run_to(10_000);
    assert!(s.recording_of(20), "setup: B recorded");
    let mark = s.now;
    s.settings_changed();
    assert_eq!(s.spawns_since(mark - Duration::from_millis(1)), 1, "one restart");
    assert!(s.recording_of(20) && s.pending_restart.is_none());
    assert_eq!(s.island_said.last(), Some(&settings_notice()));
}

/// U2c: a change during a save waits for the save, then restarts once (with the notice).
#[test]
fn u2c_a_settings_change_during_a_save_restarts_after_it() {
    let mut s = Sim::new();
    s.focus(game_ev(VAL, 10, 0x10));
    s.run_to(3_000);
    assert!(s.save());
    let mark = s.now;
    s.settings_changed();
    s.run_to(3_500);
    assert!(s.ring_kept_since(mark) && s.pending_restart == Some(RestartReason::Settings), "held while the save runs");
    s.saved();
    s.run_to(3_600);
    assert_eq!(s.spawns_since(mark), 1, "restarted once the save landed");
    assert!(s.pending_restart.is_none());
    assert_eq!(s.island_said, vec![settings_notice()]);
}

/// U2d: a drain that ends in desktop capture. The desktop engine's start reads the current
/// settings, so it is the deferred restart: one spawn, no second one, no game notice.
#[test]
fn u2d_a_drain_ending_in_desktop_capture_starts_it_once() {
    for (kind, restart) in KINDS {
        let mut s = draining_a(true);
        let mark = s.now;
        restart(&mut s);
        assert!(s.ring_kept_since(mark) && s.pending_restart.is_some(), "{kind}: waits for the drain");
        s.run_to(60_000);
        save_last_clip(&mut s, kind);
        s.run_to(126_000);
        assert_eq!(*s.engine(), EngineTarget::Desktop, "{kind}: drain over: desktop capture");
        assert_eq!(s.pending_restart, None, "{kind}: the desktop start applied the settings");
        s.run_to(130_000);
        assert_eq!(s.spawns_since(mark), 1, "{kind}: one desktop start, no extra restart");
        assert!(s.island_said.is_empty(), "{kind}: no game buffer wiped by a restart");
    }
}

/// U2e: another game cuts the drain. Its engine's start reads the current settings: one
/// spawn for B, no second restart after it; A's clip was saved before B committed.
#[test]
fn u2e_a_drain_cut_by_another_game_starts_it_once() {
    for (kind, restart) in KINDS {
        let mut s = draining_a(false);
        let mark = s.now;
        restart(&mut s);
        assert!(s.ring_kept_since(mark) && s.pending_restart.is_some(), "{kind}: waits for the drain");
        s.run_to(20_000);
        save_last_clip(&mut s, kind);
        s.focus(game_ev(APEX, 20, 0x20));
        s.run_to(30_000);
        assert!(s.recording_of(20), "{kind}: B recorded");
        assert_eq!(s.pending_restart, None, "{kind}: B's start applied the settings");
        assert_eq!(s.spawns_since(mark), 1, "{kind}: one start for B, no extra restart");
        assert!(!s.island_said.contains(&settings_notice()), "{kind}: B's buffer was never wiped by a restart");
    }
}

/// U2f: the drain ends with the engine kept (A's window is gone, then the same window is the
/// game again). The deferred restart runs then, with its own wording: "Settings applied"
/// for a settings change, nothing for a resume (a resume restart has no notice).
#[test]
fn u2f_a_deferred_restart_runs_when_the_drain_ends_with_the_engine_kept() {
    for (kind, restart) in KINDS {
        let mut s = Sim::new();
        s.focus(game_ev(VAL, 10, 0x10));
        s.run_to(5_000);
        s.window_gone(0x10);
        s.run_to(5_100);
        assert!(s.planner.drain().is_some(), "{kind}: setup: A's window drains");
        let mark = s.now;
        restart(&mut s);
        assert!(s.ring_kept_since(mark) && s.pending_restart.is_some(), "{kind}: waits for the drain");
        s.focus(game_ev(VAL, 10, 0x10));
        s.run_to(8_000);
        assert!(s.recording_of(10) && s.planner.drain().is_none(), "{kind}: the same window is the game again");
        assert_eq!(s.spawns_since(mark), 1, "{kind}: the deferred restart ran once");
        assert_eq!(s.pending_restart, None, "{kind}");
        let said = s.island_said.contains(&settings_notice());
        assert_eq!(said, kind == "settings", "{kind}: 'Settings applied' only for a settings change: {:?}", s.island_said);
    }
}

/// U2g (merge-gate P1 on 304c032): an encoder-recovery hang restart during a drain restarts
/// at once. `EncoderRecovery` records the hang restart as done when it answers `Restart`; if
/// the engine stayed (deferred), the still-hung engine reads as "hung again" -> a false
/// "Recording stopped" and no more restarts. Here the engine beats only once replaced.
#[test]
fn u2g_a_hang_restart_during_a_drain_restarts_at_once() {
    let mut s = draining_a(true);
    let mark = s.now;
    let mut rec = EncoderRecovery::default();
    let up = EngineHealth::Running { pid: 9 };
    let mut answers = Vec::new();
    for sec in 0..=120u64 {
        let replaced = !s.ring_kept_since(mark);
        let progress = if replaced { sec * 10 } else { 3 }; // the old engine is hung
        let r = rec.step(s.now, "recording", up, progress, true, false);
        if r == Recovery::Restart {
            assert_eq!(s.restart(RestartReason::Recovery), Restarted::Now, "a recovery restart never waits");
            assert!(s.pending_restart.is_none() && s.planner.drain().is_none(), "the hung engine is replaced now");
        }
        if r != Recovery::Nothing {
            answers.push((sec, r));
        }
        let next = s.now + Duration::from_secs(1) - s.t0;
        s.run_to(next.as_millis() as u64);
    }
    assert_eq!(answers.first().map(|a| a.1), Some(Recovery::Restart), "{answers:?}");
    assert!(!answers.iter().any(|a| a.1 == Recovery::GaveUp), "no false 'Recording stopped': {answers:?}");
    assert_eq!(*s.engine(), EngineTarget::Desktop);
}

/// The Sim runs the controller's restart code (`record_restart`), not a copy: the
/// controller's log lines come out of it, and a deferred restart logs once, not every tick.
#[test]
fn the_sim_runs_the_controllers_restart_code() {
    let mut s = draining_a(false);
    s.settings_changed();
    s.run_to(60_000);
    let deferred = s.notes.iter().filter(|l| l.contains("restart deferred until the closed game's drain ends")).count();
    assert_eq!(deferred, 1, "one deferral line: {:?}", s.notes);
    s.restart(RestartReason::Resume);
    assert_eq!(s.notes.iter().filter(|l| l.contains("deferred")).count(), 1, "already pending: {:?}", s.notes);
    assert_eq!(s.pending_restart, Some(RestartReason::Settings), "a settings change stays the reason");
}
