//! The privacy release's settle rules on the awake-only clock (design-detect-release §3: P2,
//! P7, P8), driven through the shared transition rules (`Transitions::drop_ring`) in the
//! harness, with the engine's drain lines fed as the supervisor forwards them.

use std::time::Duration;

use crate::transitions_sim::{app_ev, Sim};

const S: fn(u64) -> Duration = Duration::from_secs;

/// Fullscreen Discord (pid 30) recorded, a save sent at 5 s, then Esc: released for the save.
fn released_discord(heartbeats: bool) -> Sim {
    let mut s = Sim::new();
    s.engine_heartbeats = heartbeats;
    s.focus(app_ev("Discord", 30, 0x30, true));
    s.run_to(5_000);
    assert!(s.recording_of(30) && s.save(), "setup: recording, a save sent");
    s.seen(app_ev("Discord", 30, 0x30, false));
    assert_eq!(s.released.len(), 1, "setup: released for the save");
    s
}

/// The engine's drain line (`save_progress`), as the supervisor forwards it.
fn drain_line(s: &mut Sim, step: u64, stalled_for: u64) {
    let ev = serde_json::json!({"type": "save_progress", "save_writing": {"bytes": step, "queued": 0, "step": step}, "stalled_for": stalled_for});
    let now = s.awake();
    s.activity.on_engine_event(&ev, 0.0, now);
}

/// P2: the release is stamped by the shared rule (so the Sim runs the controller's stamp, p6-2)
/// at the release, with the hello's heartbeat capability.
#[test]
fn the_shared_rule_stamps_the_release_with_the_engines_heartbeat_capability() {
    for beats in [true, false] {
        let s = released_discord(beats);
        assert_eq!(s.activity.release(), Some((s.awake(), beats)), "heartbeats: {beats}");
    }
}

/// P7: a heartbeat engine is never settled on a timer: an hour of awake time with its save
/// stalled (it judges that itself) and beats every second; only the answer settles it.
#[test]
fn a_heartbeat_engine_is_settled_only_by_its_answers() {
    let mut s = released_discord(true);
    for k in 1..=3600u64 {
        s.now += S(1);
        drain_line(&mut s, 1, k);
        let now = s.awake();
        let _ = s.activity.retarget_busy(now); // `plan_engine`'s read: expiry and the cap run here
        assert!(!s.activity.release_may_settle(now), "settled at release + {k} s on a stall timer");
    }
    s.saved();
    let now = s.awake();
    assert!(s.activity.release_may_settle(now), "answered: settles");
}

/// P8, no heartbeats: the save stalled 119 s before the stop. The settle counts 135 s from the
/// release (not at once, p5-1), and progress seen at release + 120.5 s (the engine's clock
/// starts a little later, p6-1) keeps it.
#[test]
fn an_old_engine_settles_135_s_after_the_later_of_the_release_and_its_progress() {
    let mut s = Sim::new();
    s.engine_heartbeats = false;
    s.focus(app_ev("Discord", 30, 0x30, true));
    s.run_to(5_000);
    assert!(s.save());
    s.now += S(119); // no progress since the save was sent
    s.seen(app_ev("Discord", 30, 0x30, false));
    assert_eq!(s.released.len(), 1);
    let stop = s.awake();
    assert!(!s.activity.release_may_settle(stop + S(1)), "settled at once: killed at stop + 8 s");
    assert!(!s.activity.release_may_settle(stop + S(134)));
    assert!(s.activity.release_may_settle(stop + S(135)), "135 s after the release, no progress");
    s.now += Duration::from_millis(120_500);
    drain_line(&mut s, 2, 0);
    assert!(!s.activity.release_may_settle(stop + S(121)), "progress at release + 120.5 s");
    assert!(!s.activity.release_may_settle(stop + S(255)));
    assert!(s.activity.release_may_settle(stop + S(256)), "135 s after that progress");
}

/// P7/P8 across a sleep: the awake clock stops, so a 2 h sleep during the drain settles
/// nothing, heartbeats or not.
#[test]
fn a_sleep_during_the_drain_settles_nothing() {
    for beats in [true, false] {
        let mut s = released_discord(beats);
        s.now += S(30);
        drain_line(&mut s, 3, 0);
        s.sleep_for(S(2 * 3600));
        s.tick(); // the first pass after the wake, before any resume notice
        let now = s.awake();
        let _ = s.activity.retarget_busy(now);
        assert!(!s.activity.release_may_settle(now), "settled by a sleep (heartbeats: {beats})");
    }
}

/// P9, an engine without `stop_capture`: the stop is held for its save (F-detect-1) and saves
/// are refused, on the awake clock: a 1 h sleep during the hold does not let the stop cut it.
#[test]
fn an_old_engines_held_stop_survives_a_sleep() {
    let mut s = Sim::new();
    s.engine_releases = false;
    s.focus(app_ev("Discord", 30, 0x30, true));
    s.run_to(5_000);
    assert!(s.save());
    s.seen(app_ev("Discord", 30, 0x30, false));
    let mark = s.now;
    s.run_to(5_500);
    s.sleep_for(S(3600));
    s.tick();
    s.now += S(1);
    s.tick();
    assert!(s.ring_kept_since(mark), "the held stop cut the save after the sleep");
    assert!(s.planner.privacy_refusal().is_some(), "still held: new saves refused");
    s.saved();
    s.now += S(1);
    s.tick();
    assert!(!s.ring_kept_since(mark), "the save landed: the stop applies");
}

/// P13: a save that expired (no answer for 120 s awake) is still reported when the engine
/// exits: "Save failed", never zeroed silently. The release ends with the engine.
#[test]
fn an_engine_exit_reports_an_expired_save_as_lost() {
    use crate::record_activity::Trigger;
    let mut s = released_discord(true);
    s.now += S(121);
    let now = s.awake();
    assert!(!s.activity.retarget_busy(now).saving, "setup: the save expired (counted as lost by the timers)");
    let exited = serde_json::json!({"type": crate::supervisor::ENGINE_EXITED});
    assert_eq!(s.activity.on_engine_event(&exited, 0.0, now), Some(Trigger::SavesLost(1)), "reported");
    assert_eq!(s.activity.release(), None, "the released engine is gone");
    assert_eq!(s.activity.on_engine_event(&exited, 0.0, now), None, "once");
}

/// P17: a release still draining while the next game is wanted: no notice for 29 s, exactly
/// one at 30 s with the product wording, none after the engine exits.
#[test]
fn a_drain_that_blocks_the_next_game_says_so_once_after_30_s() {
    use crate::transitions_sim::game_ev;
    let mut s = released_discord(true);
    let stop = s.now;
    s.focus(game_ev("Apex", 40, 0x40));
    let ms = |s: &Sim, d: Duration| (stop + d).duration_since(s.t0).as_millis() as u64;
    s.run_to(ms(&s, S(29)));
    let told = |s: &Sim| s.island_said.iter().filter(|(t, _)| t == "Finishing the last clip").count();
    assert_eq!(told(&s), 0, "not before 30 s");
    s.run_to(ms(&s, S(31)));
    assert_eq!(told(&s), 1);
    assert!(s.island_said.contains(&("Finishing the last clip".into(), "Apex starts recording once it is saved".into())), "{:?}", s.island_said);
    s.run_to(ms(&s, S(90)));
    assert_eq!(told(&s), 1, "once per release");
    s.saved();
    let _ = s.engine_exited();
    s.run_to(ms(&s, S(200)));
    assert_eq!(told(&s), 1, "the drain ended: no further notice");
}
