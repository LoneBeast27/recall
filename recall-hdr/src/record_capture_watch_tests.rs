use super::*;
use crate::record_target::Target;
use crate::supervisor::EngineHealth;
use serde_json::Value;

fn game(name: &str, hwnd: isize) -> EngineTarget {
    EngineTarget::Window(Target { hwnd, pid: 42, name: name.into(), exe: format!("{name}.exe") })
}

const RUN: EngineHealth = EngineHealth::Running { pid: 9 };

/// Feed (second, target, health, encoder, fresh) steps through `session_state`
/// into the watch; return the seconds that acted and whether each had a notice.
fn run(steps: &[Step]) -> Vec<(u64, Option<Value>)> {
    let t0 = Instant::now();
    let mut w = StuckWatch::default();
    steps
        .iter()
        .filter_map(|(s, target, health, encoder, fresh)| {
            let state = session_state(target, *health, *encoder, *fresh, false);
            let running = matches!(health, EngineHealth::Running { pid } if *pid != 0);
            let act = w.observe(state, target, running, *fresh, true, t0 + Duration::from_secs(*s))?;
            Some((*s, act.notice.map(|n| serde_json::from_str(&n).unwrap())))
        })
        .collect()
}

/// One step per second from `from` to `to`, all with the same inputs.
type Step = (u64, EngineTarget, EngineHealth, Option<&'static str>, bool);

fn hold(from: u64, to: u64, t: &EngineTarget, h: EngineHealth, enc: Option<&'static str>, fresh: bool) -> Vec<Step> {
    (from..=to).map(|s| (s, t.clone(), h, enc, fresh)).collect()
}

#[test]
fn a_game_starting_for_over_10_s_gets_one_notice_and_one_restart() {
    let g = game("VALORANT", 7);
    let acts = run(&hold(0, 15, &g, RUN, Some("starting"), true));
    assert_eq!(acts.len(), 1, "{acts:?}");
    assert_eq!(acts[0].0, 10);
    assert_eq!(
        acts[0].1,
        Some(serde_json::json!({"type":"notice","kind":"refused","title":"Can't capture VALORANT","detail":"Retrying"}))
    );
}

#[test]
fn after_the_restart_it_never_loops_faster_than_60_s_and_says_it_once() {
    let g = game("VALORANT", 7);
    let mut steps = hold(0, 10, &g, RUN, Some("starting"), true);
    // The intended restart: no hello yet for 2 s, then fresh again, still no frames.
    steps.extend(hold(11, 12, &g, RUN, None, false));
    steps.extend(hold(13, 200, &g, RUN, Some("starting"), true));
    let acts = run(&steps);
    let secs: Vec<u64> = acts.iter().map(|a| a.0).collect();
    assert_eq!(secs, [10, 70, 130, 190]);
    assert!(acts[1..].iter().all(|a| a.1.is_none()), "the notice is said once");
}

#[test]
fn not_stuck_while_recording_stale_spawning_desktop_or_paused() {
    let g = game("Apex", 7);
    assert!(run(&hold(0, 30, &g, RUN, Some("h264_nvenc"), true)).is_empty(), "recording");
    assert!(run(&hold(0, 30, &g, RUN, Some("starting"), false)).is_empty(), "no fresh hello: the old engine");
    assert!(run(&hold(0, 30, &g, EngineHealth::Running { pid: 0 }, None, true)).is_empty(), "spawning");
    assert!(run(&hold(0, 30, &g, EngineHealth::Restarting { attempt: 1 }, None, true)).is_empty(), "crash backoff");
    assert!(run(&hold(0, 30, &EngineTarget::Desktop, RUN, Some("starting"), true)).is_empty(), "desktop");
    let t0 = Instant::now();
    let mut w = StuckWatch::default();
    for s in 0..30 {
        let state = session_state(&g, RUN, Some("starting"), true, true);
        assert!(w.observe(state, &g, true, true, true, t0 + Duration::from_secs(s)).is_none(), "paused");
    }
}

#[test]
fn a_new_target_or_a_recovery_makes_a_later_stall_news_again() {
    let a = game("A", 7);
    let b = game("B", 8);
    let mut steps = hold(0, 10, &a, RUN, Some("starting"), true);
    steps.extend(hold(11, 25, &b, RUN, Some("starting"), true));
    let acts = run(&steps);
    assert_eq!(acts.iter().map(|a| a.0).collect::<Vec<_>>(), [10, 21]);
    assert!(acts.iter().all(|a| a.1.is_some()));

    let mut steps = hold(0, 10, &a, RUN, Some("starting"), true);
    steps.extend(hold(11, 20, &a, RUN, Some("h264_nvenc"), true));
    steps.extend(hold(21, 40, &a, RUN, Some("starting"), true));
    let acts = run(&steps);
    assert_eq!(acts.iter().map(|a| a.0).collect::<Vec<_>>(), [10, 31]);
    assert!(acts[1].1.is_some(), "recovered, then stuck again: told again");
}

#[test]
fn a_flicker_out_of_starting_restarts_the_10_s_clock() {
    let g = game("A", 7);
    let mut steps = hold(0, 8, &g, RUN, Some("starting"), true);
    steps.extend(hold(9, 9, &g, RUN, Some("starting"), false));
    steps.extend(hold(10, 25, &g, RUN, Some("starting"), true));
    assert_eq!(run(&steps).iter().map(|a| a.0).collect::<Vec<_>>(), [20]);
}

// capture_stuck_tick's step (`stuck_step`): the controller's inputs, the screen probe.

fn inputs(t: &EngineTarget) -> StuckInputs<'_> {
    StuckInputs { wanted: t, health: RUN, encoder: Some("starting"), fresh: true, paused: false, running: true, held: false, overlay: true }
}

/// A game stuck in "starting" for seconds `0..=to`, the screen as `sight(second)` says;
/// returns (second, has notice) per act (each act is one engine restart).
fn tick_run(to: u64, i: &StuckInputs, sight: impl Fn(u64) -> Sight) -> Vec<(u64, bool)> {
    let t0 = Instant::now();
    let mut w = StuckWatch::default();
    (0..=to)
        .filter_map(|s| {
            let act = stuck_step(&mut w, i, |_: &Target| sight(s), t0 + Duration::from_secs(s))?;
            Some((s, act.notice.is_some()))
        })
        .collect()
}

#[test]
fn no_notice_and_no_restart_while_the_game_is_minimized_behind_a_window_locked_or_the_display_is_off() {
    let g = game("VALORANT", 7);
    let i = inputs(&g);
    let away = [
        ("minimized", Sight { iconic: true, in_front: false, ..Sight::SHOWN }),
        ("behind another window", Sight { in_front: false, ..Sight::SHOWN }),
        ("locked", Sight { locked: true, in_front: false, ..Sight::SHOWN }),
        ("display off", Sight { display_off: true, ..Sight::SHOWN }),
    ];
    for (why, sight) in away {
        assert_eq!(tick_run(60, &i, |_| sight), [], "{why}: 60 s");
    }
}

#[test]
fn in_front_and_stuck_for_10_s_still_gets_one_notice_and_one_restart() {
    let g = game("VALORANT", 7);
    assert_eq!(tick_run(15, &inputs(&g), |_| Sight::SHOWN), [(10, true)]);
}

#[test]
fn the_10_s_count_starts_when_the_game_is_back_in_front() {
    let g = game("VALORANT", 7);
    let back_at_30 = |s| if s < 30 { Sight { iconic: true, in_front: false, ..Sight::SHOWN } } else { Sight::SHOWN };
    assert_eq!(tick_run(45, &inputs(&g), back_at_30), [(40, true)]);
}

#[test]
fn a_drain_or_a_busy_engine_holds_it_and_the_overlay_off_drops_only_the_notice() {
    let g = game("VALORANT", 7);
    assert_eq!(tick_run(30, &StuckInputs { held: true, ..inputs(&g) }, |_| Sight::SHOWN), []);
    assert_eq!(tick_run(15, &StuckInputs { overlay: false, ..inputs(&g) }, |_| Sight::SHOWN), [(10, false)]);
}

#[test]
fn the_screen_is_not_asked_for_the_desktop_or_a_recording_game() {
    let probe = |_: u64| -> Sight { panic!("asked") };
    assert_eq!(tick_run(20, &inputs(&EngineTarget::Desktop), probe), []);
    let g = game("A", 7);
    assert_eq!(tick_run(20, &StuckInputs { encoder: Some("h264_nvenc"), ..inputs(&g) }, probe), []);
}
