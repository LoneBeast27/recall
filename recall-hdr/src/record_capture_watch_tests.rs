use super::*;
use crate::record_screen::Frame;
use crate::record_target::Target;
use crate::supervisor::EngineHealth;
use serde_json::Value;

/// A game whose process id is its window handle (another window = another game).
fn game(name: &str, hwnd: isize) -> EngineTarget {
    game_of(name, hwnd, hwnd as u32)
}

fn game_of(name: &str, hwnd: isize, pid: u32) -> EngineTarget {
    EngineTarget::Window(Target { hwnd, pid, name: name.into(), exe: format!("{name}.exe") })
}

const RUN: EngineHealth = EngineHealth::Running { pid: 9 };

/// What an act did: "restart", "notice", or "quiet" (a notice the overlay-off dropped).
fn kind(act: &StuckAct) -> &'static str {
    match (act.restart, &act.notice) {
        (true, _) => "restart",
        (false, Some(_)) => "notice",
        (false, None) => "quiet",
    }
}

/// Feed (second, target, health, encoder, fresh) steps through `session_state`
/// into the watch; return (second, kind, notice) per act.
fn run(steps: &[Step]) -> Vec<(u64, &'static str, Option<Value>)> {
    let t0 = Instant::now();
    let mut w = StuckWatch::default();
    steps
        .iter()
        .filter_map(|(s, target, health, encoder, fresh)| {
            let state = session_state(target, *health, *encoder, *fresh, false);
            let running = matches!(health, EngineHealth::Running { pid } if *pid != 0);
            let act = w.observe(state, target, running, *fresh, true, t0 + Duration::from_secs(*s))?;
            Some((*s, kind(&act), act.notice.map(|n| serde_json::from_str(&n).unwrap())))
        })
        .collect()
}

fn when(acts: &[(u64, &'static str, Option<Value>)]) -> Vec<(u64, &'static str)> {
    acts.iter().map(|a| (a.0, a.1)).collect()
}

/// One step per second from `from` to `to`, all with the same inputs.
type Step = (u64, EngineTarget, EngineHealth, Option<&'static str>, bool);

fn hold(from: u64, to: u64, t: &EngineTarget, h: EngineHealth, enc: Option<&'static str>, fresh: bool) -> Vec<Step> {
    (from..=to).map(|s| (s, t.clone(), h, enc, fresh)).collect()
}

#[test]
fn a_game_starting_for_over_10_s_gets_one_restart() {
    let g = game("VALORANT", 7);
    assert_eq!(when(&run(&hold(0, 15, &g, RUN, Some("starting"), true))), [(10, "restart")]);
}

#[test]
fn after_the_one_restart_it_says_so_once_and_never_restarts_again() {
    let g = game("VALORANT", 7);
    let mut steps = hold(0, 10, &g, RUN, Some("starting"), true);
    // The intended restart: no hello yet for 2 s, then fresh again, still no frames.
    steps.extend(hold(11, 12, &g, RUN, None, false));
    steps.extend(hold(13, 300, &g, RUN, Some("starting"), true));
    let acts = run(&steps);
    assert_eq!(when(&acts), [(10, "restart"), (23, "notice")]);
    assert_eq!(
        acts[1].2,
        Some(serde_json::json!({"type":"notice","kind":"refused","title":"Can't capture VALORANT","detail":"Try another window mode"}))
    );
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
    assert_eq!(when(&run(&steps)), [(10, "restart"), (21, "restart")]);

    let mut steps = hold(0, 10, &a, RUN, Some("starting"), true);
    steps.extend(hold(11, 20, &a, RUN, Some("h264_nvenc"), true));
    steps.extend(hold(21, 40, &a, RUN, Some("starting"), true));
    assert_eq!(when(&run(&steps)), [(10, "restart"), (31, "restart")], "recovered, then stuck again: a fresh episode");
}

#[test]
fn a_flicker_out_of_starting_restarts_the_10_s_clock() {
    let g = game("A", 7);
    let mut steps = hold(0, 8, &g, RUN, Some("starting"), true);
    steps.extend(hold(9, 9, &g, RUN, Some("starting"), false));
    steps.extend(hold(10, 25, &g, RUN, Some("starting"), true));
    assert_eq!(when(&run(&steps)), [(20, "restart")]);
}

// capture_stuck_tick's step (`stuck_step`): the controller's inputs, the screen probe.

fn inputs(t: &EngineTarget) -> StuckInputs<'_> {
    StuckInputs {
        wanted: t,
        health: RUN,
        encoder: Some("starting"),
        fresh: true,
        paused: false,
        running: true,
        held: false,
        overlay: true,
    }
}

/// `stuck_step` once a second for `0..=to`, inputs and screen as `inputs(second)` /
/// `sight(second)` say (the screen only when asked); returns (second, kind) per act.
fn tick_seq<'a>(to: u64, inputs: impl Fn(u64) -> StuckInputs<'a>, sight: impl Fn(u64) -> Sight) -> Vec<(u64, &'static str)> {
    let t0 = Instant::now();
    let mut w = StuckWatch::default();
    (0..=to)
        .filter_map(|s| {
            let act = stuck_step(&mut w, &inputs(s), |_: &Target| sight(s), t0 + Duration::from_secs(s))?;
            Some((s, kind(&act)))
        })
        .collect()
}

/// The same inputs every second, the screen as `sight(second)` says.
fn tick_run(to: u64, i: &StuckInputs, sight: impl Fn(u64) -> Sight) -> Vec<(u64, &'static str)> {
    tick_seq(to, |_| i.clone(), sight)
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
fn in_front_and_stuck_for_10_s_still_gets_one_restart() {
    let g = game("VALORANT", 7);
    assert_eq!(tick_run(15, &inputs(&g), |_| Sight::SHOWN), [(10, "restart")]);
}

#[test]
fn a_first_frame_that_never_comes_gets_at_most_one_restart_then_one_notice_in_5_minutes() {
    let g = game("VALORANT", 7);
    // The next 10 s count from the second after the restart.
    assert_eq!(tick_run(300, &inputs(&g), |_| Sight::SHOWN), [(10, "restart"), (21, "notice")]);
}

#[test]
fn a_new_window_mode_starts_a_new_episode() {
    let g = game("VALORANT", 7);
    let borderless_at_100 = |s| if s < 100 { windowed(1920, 1080) } else { fullscreen() };
    assert_eq!(tick_run(130, &inputs(&g), borderless_at_100), REARMED_AT_100);
}

#[test]
fn the_10_s_count_starts_when_the_game_is_back_in_front() {
    let g = game("VALORANT", 7);
    let back_at_30 = |s| if s < 30 { Sight { iconic: true, in_front: false, ..Sight::SHOWN } } else { Sight::SHOWN };
    assert_eq!(tick_run(45, &inputs(&g), back_at_30), [(40, "restart")]);
}

#[test]
fn a_drain_a_busy_engine_or_a_pending_recovery_holds_it_and_the_overlay_off_drops_only_the_notice() {
    let g = game("VALORANT", 7);
    assert_eq!(tick_run(30, &StuckInputs { held: true, ..inputs(&g) }, |_| Sight::SHOWN), []);
    assert_eq!(tick_run(25, &StuckInputs { overlay: false, ..inputs(&g) }, |_| Sight::SHOWN), [(10, "restart"), (21, "quiet")]);
}

#[test]
fn a_restart_in_flight_is_pending_so_the_encoder_recovery_stands_aside() {
    let g = game("VALORANT", 7);
    let t0 = Instant::now();
    let mut w = StuckWatch::default();
    let at = |s| t0 + Duration::from_secs(s);
    assert!(!w.pending(at(0)));
    assert!(stuck_step(&mut w, &inputs(&g), |_: &Target| Sight::SHOWN, at(0)).is_none());
    assert!(w.pending(at(0)), "the clock runs");
    assert!(stuck_step(&mut w, &inputs(&g), |_: &Target| Sight::SHOWN, at(10)).is_some_and(|a| a.restart));
    assert!(w.pending(at(10)), "the restart awaits its first frame");
    for s in 11..=21 {
        stuck_step(&mut w, &inputs(&g), |_: &Target| Sight::SHOWN, at(s));
    }
    let recording = StuckInputs { encoder: Some("h264_nvenc"), ..inputs(&g) };
    stuck_step(&mut w, &recording, |_: &Target| Sight::SHOWN, at(22));
    assert!(!w.pending(at(22)), "told, then recording: nothing in flight");
}

#[test]
fn the_screen_is_not_asked_for_the_desktop_or_a_recording_game() {
    let probe = |_: u64| -> Sight { panic!("asked") };
    assert_eq!(tick_run(20, &inputs(&EngineTarget::Desktop), probe), []);
    let g = game("A", 7);
    assert_eq!(tick_run(20, &StuckInputs { encoder: Some("h264_nvenc"), ..inputs(&g) }, probe), []);
}

/// Review 11-07: a restart whose engine comes back in `error` (never `starting`) must not
/// hold the encoder recovery off for good; the restart counts as in flight for RESTART_GRACE only.
#[test]
fn a_restart_counts_as_in_flight_for_a_bounded_time_only() {
    let g = game("VALORANT", 7);
    let t0 = Instant::now();
    let mut w = StuckWatch::default();
    let at = |s| t0 + Duration::from_secs(s);
    stuck_step(&mut w, &inputs(&g), |_: &Target| Sight::SHOWN, at(0));
    assert!(stuck_step(&mut w, &inputs(&g), |_: &Target| Sight::SHOWN, at(10)).is_some_and(|a| a.restart));
    // The new engine reports an encoder error: stuck no longer observes `starting`.
    let failed = StuckInputs { encoder: Some("failed"), ..inputs(&g) };
    stuck_step(&mut w, &failed, |_: &Target| Sight::SHOWN, at(11));
    assert!(w.pending(at(11)), "just restarted: recovery waits");
    assert!(!w.pending(at(10) + RESTART_GRACE), "bounded: recovery may act");
}

// p2 11-07: after the notice only a target change or a window-mode change re-arms it.

const TITLED: u32 = 0x00C0_0000 | 0x0004_0000; // WS_CAPTION | WS_THICKFRAME

/// The game in a titled window of this size on a 2560x1440 monitor.
fn windowed(w: i32, h: i32) -> Sight {
    Sight { frame: Frame { style: TITLED, rect: [100, 100, 100 + w, 100 + h], monitor: [0, 0, 2560, 1440] }, ..Sight::SHOWN }
}

fn fullscreen() -> Sight {
    Sight::SHOWN
}

const TOLD: [(u64, &str); 2] = [(10, "restart"), (21, "notice")];
const REARMED_AT_100: [(u64, &str); 4] = [(10, "restart"), (21, "notice"), (110, "restart"), (121, "notice")];

#[test]
fn a_shortcut_press_after_the_notice_does_not_rearm() {
    // A save hotkey at 100 s: the engine is busy saving for 2 s. The controller's last
    // shortcut time (`last_fired`) is no input any more.
    let g = game("VALORANT", 7);
    let pressed = |s| StuckInputs { held: (100..102).contains(&s), ..inputs(&g) };
    assert_eq!(tick_seq(200, pressed, |_| windowed(1920, 1080)), TOLD);
}

#[test]
fn a_pause_and_resume_after_the_notice_do_not_rearm() {
    let g = game("VALORANT", 7);
    let paused = |s: u64| StuckInputs { paused: (100..105).contains(&s), ..inputs(&g) };
    assert_eq!(tick_seq(200, paused, |_| windowed(1920, 1080)), TOLD);
}

#[test]
fn a_window_resize_does_not_rearm() {
    let g = game("VALORANT", 7);
    let resized = |s| if s < 100 { windowed(1920, 1080) } else { windowed(1600, 900) };
    assert_eq!(tick_run(200, &inputs(&g), resized), TOLD);
}

#[test]
fn a_target_change_rearms() {
    let a = game_of("A", 7, 42);
    let b = game_of("B", 8, 43);
    let switched = |s| inputs(if s < 100 { &a } else { &b });
    assert_eq!(tick_seq(200, switched, |_| windowed(1920, 1080)), REARMED_AT_100);
}

#[test]
fn a_fullscreen_switch_rearms() {
    let g = game("VALORANT", 7);
    let switched = |s| if s < 100 { windowed(1920, 1080) } else { fullscreen() };
    assert_eq!(tick_run(200, &inputs(&g), switched), REARMED_AT_100);
    let back = |s| if s < 100 { fullscreen() } else { windowed(1920, 1080) };
    assert_eq!(tick_run(200, &inputs(&g), back), REARMED_AT_100, "and back");
}

#[test]
fn a_new_window_of_the_same_game_rearms_only_if_its_fullscreen_state_differs() {
    let old = game_of("VALORANT", 7, 42);
    let new = game_of("VALORANT", 9, 42);
    let moved = |s| inputs(if s < 100 { &old } else { &new });
    assert_eq!(tick_seq(200, moved, |_| fullscreen()), TOLD, "same mode: not news");
    let sight = |s| if s < 100 { fullscreen() } else { windowed(1920, 1080) };
    assert_eq!(tick_seq(200, moved, sight), REARMED_AT_100, "fullscreen -> windowed");
}

#[test]
fn a_static_game_with_a_long_recording_gets_nothing_for_5_minutes() {
    // The engine says `recording` whether frames come or not (VFR); a long recording keeps
    // it busy (`held`). Neither asks the screen nor restarts anything.
    let g = game("VALORANT", 7);
    let probe = |_: u64| -> Sight { panic!("asked") };
    let recording = StuckInputs { encoder: Some("h264_nvenc"), ..inputs(&g) };
    assert_eq!(tick_run(300, &StuckInputs { held: true, ..recording.clone() }, probe), []);
    assert_eq!(tick_run(300, &recording, probe), []);
}
