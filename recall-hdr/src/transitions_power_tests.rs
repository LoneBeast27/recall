//! Sleep and resume through the `PowerHost` seam (design-detect-release P14/P15), in the
//! transition harness: the notices queue as the system's callback queues them, the pump runs
//! `power_tick` before `tick`, and the Sim's awake clock stops while it sleeps (`sleep_for`).

use std::time::{Duration, Instant};

use crate::record_power::{PBT_RESUME_AUTOMATIC, PBT_SUSPEND};
use crate::record_power_host::{resume_lost_event, PowerHost};
use crate::transitions_sim::{app_ev, game_ev, Sim, Switch};

const S: fn(u64) -> Duration = Duration::from_secs;

/// A game recorded, a save sent at 5 s. Returns the mark before any resume.
fn saving_game() -> (Sim, Instant) {
    let mut s = Sim::new();
    s.focus(game_ev("Apex", 40, 0x40));
    s.run_to(5_000);
    assert!(s.recording_of(40) && s.save(), "setup: recording, a save sent");
    let mark = s.now;
    (s, mark)
}

/// One pump pass: the power notices, then the tick.
fn pump(s: &mut Sim) {
    s.power_tick();
    s.tick();
}

/// The engine reports the save it writes (`status.save_writing`, the mark moving).
fn writing(s: &mut Sim, step: u64) {
    let ev = serde_json::json!({"type": "status", "save_writing": {"bytes": step, "queued": 0, "step": step}});
    let now = s.awake();
    s.activity.on_engine_event(&ev, 0.0, now);
}

/// Planned restarts the supervisor was asked for (a resume's: `Sent::Restarting`).
fn resume_restarts(s: &Sim) -> usize {
    s.switches.0.borrow().iter().filter(|c| matches!(c, Switch::Restart { planned: true, .. })).count()
}

/// P14 + P15(1): a save in flight (an engine with no `save_writing` report yet) over a 2 h
/// sleep. The pump runs a tick BEFORE the resume notice arrives (B11). Nothing aged, so the
/// restart holds for the save; it goes once the save is answered, and nothing is reported lost.
#[test]
fn a_save_in_flight_across_a_long_sleep_holds_the_resume_restart_until_it_lands() {
    let (mut s, mark) = saving_game();
    s.power_notice(PBT_SUSPEND);
    pump(&mut s);
    s.sleep_for(S(2 * 3600));
    s.tick(); // the pump woke on its own before the resume notice was queued
    s.power_notice(PBT_RESUME_AUTOMATIC);
    pump(&mut s);
    for _ in 0..20 {
        s.now += S(1);
        pump(&mut s);
    }
    assert!(s.ring_kept_since(mark) && resume_restarts(&s) == 0, "the restart would have cut the save");
    assert!(s.power.held(), "held for the save");
    s.saved();
    pump(&mut s);
    assert_eq!(resume_restarts(&s), 1, "the save landed: restart now");
    assert!(s.hub_events.is_empty(), "nothing was lost");
}

/// P15(2), product rule: past its 10 min cap a save that still moves keeps the hold; once it
/// stops for 120 s awake the restart goes and the save is reported exactly once.
#[test]
fn a_save_past_its_cap_holds_while_it_moves_then_is_reported_when_it_stops() {
    let (mut s, mark) = saving_game();
    writing(&mut s, 1);
    s.power_notice(PBT_SUSPEND);
    pump(&mut s);
    s.sleep_for(S(3600));
    s.power_notice(PBT_RESUME_AUTOMATIC);
    for k in 0..(15 * 60 / 5) {
        s.now += S(5);
        writing(&mut s, k + 2);
        pump(&mut s);
    }
    assert!(s.ring_kept_since(mark) && resume_restarts(&s) == 0, "15 min of progress, past the cap: still held");
    for _ in 0..119 {
        s.now += S(1);
        pump(&mut s);
    }
    assert_eq!(resume_restarts(&s), 0, "stalled 119 s: still held");
    s.now += S(2);
    pump(&mut s);
    assert_eq!(resume_restarts(&s), 1, "stalled 120 s: restart");
    assert_eq!(s.hub_events, vec![resume_lost_event()], "reported once, with its reason");
    for _ in 0..5 {
        s.now += S(1);
        pump(&mut s);
    }
    assert_eq!(s.hub_events.len(), 1, "not again");
}

/// P15(2) bound: a save that keeps moving holds the restart at most 30 min (awake), then it
/// is restarted and reported.
#[test]
fn a_save_that_never_ends_holds_the_resume_restart_at_most_30_min() {
    let (mut s, _) = saving_game();
    s.power_notice(PBT_SUSPEND);
    pump(&mut s);
    s.sleep_for(S(600));
    s.power_notice(PBT_RESUME_AUTOMATIC);
    pump(&mut s);
    let resumed = s.awake();
    let mut k = 0;
    while resume_restarts(&s) == 0 {
        s.now += S(10);
        k += 1;
        writing(&mut s, k);
        pump(&mut s);
        assert!(k < 400, "never let go");
    }
    let held = s.awake().saturating_duration_since(resumed);
    assert!(held >= S(30 * 60) && held < S(30 * 60 + 11), "{held:?}");
    assert_eq!(s.hub_events, vec![resume_lost_event()]);
}

/// P15(3): a resume during a privacy release restarts at once (only the next engine is
/// re-targeted; the released one drains on, `supervisor_release_wait`) and reports nothing.
#[test]
fn a_resume_during_a_release_restarts_the_next_engine_and_cuts_nothing() {
    let mut s = Sim::new();
    s.focus(app_ev("Discord", 30, 0x30, true));
    s.run_to(5_000);
    assert!(s.save());
    s.seen(app_ev("Discord", 30, 0x30, false));
    assert_eq!(s.released.len(), 1, "setup: released for the save");
    s.focus(game_ev("Apex", 40, 0x40));
    s.run_to(6_000);
    s.power_notice(PBT_SUSPEND);
    pump(&mut s);
    s.sleep_for(S(3600));
    s.power_notice(PBT_RESUME_AUTOMATIC);
    pump(&mut s);
    assert!(!s.power.held(), "nothing of the next engine to wait for");
    assert!(s.hub_events.is_empty(), "the released engine's save is not cut");
    assert_eq!(s.released.len(), 1, "no second release");
    let now = s.awake();
    assert!(!s.activity.release_may_settle(now), "the drain is still waited for");
}

/// P14: a suspend with a save in flight cuts nothing and settles nothing.
#[test]
fn a_suspend_never_cuts_or_settles_a_save() {
    let (mut s, mark) = saving_game();
    s.power_notice(PBT_SUSPEND);
    pump(&mut s);
    s.sleep_for(S(8 * 3600));
    pump(&mut s); // a pass before the resume notice
    assert!(s.ring_kept_since(mark) && s.hub_events.is_empty());
    let now = s.awake();
    assert!(s.activity.retarget_busy(now).saving, "the save is still counted as in flight");
}

/// R2-rel-2: the save stalls after a wake while a closed game's drain defers the resume
/// restart. Nothing is cut yet, so nothing is reported: the save may still land ("Clip saved"
/// alone, never after a "Save failed").
#[test]
fn a_resume_restart_deferred_by_a_drain_reports_nothing_and_the_save_may_still_land() {
    let (mut s, _) = saving_game(); // a save at 5 s, never moving
    s.run_to(100_000);
    s.exit(40); // the game closed: its drain runs (120 s)
    s.run_to(101_000);
    assert!(s.planner.drain().is_some(), "setup: the closed game's drain runs");
    s.power_notice(PBT_SUSPEND);
    pump(&mut s);
    s.sleep_for(S(5));
    s.power_notice(PBT_RESUME_AUTOMATIC);
    for _ in 0..30 {
        s.now += S(1);
        pump(&mut s); // the save has made no progress for 120 s awake by now: GoLost
    }
    assert!(!s.power.held(), "setup: the gate let the restart go");
    assert!(s.pending_restart.is_some(), "setup: the drain deferred it");
    assert!(s.hub_events.is_empty(), "reported while nothing was cut: {:?}", s.hub_events);
    let now = s.awake();
    assert!(s.activity.retarget_busy(now).saving || s.activity.unanswered() > 0, "the save is still counted");
}
