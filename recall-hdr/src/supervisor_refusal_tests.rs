//! The quit window's refusal follows the latest target (merge-gate P2 on the save stack):
//! a pause then a resume inside the grace, or a drain to Off then a game taking focus, says
//! "Switching games" (a game is wanted again); a pause's own window says "Recording is
//! paused" for the lines that skip the controller's pause gate (mic, set_mode, set_source).
//! Driven through the real manager (cmd.exe as the fake engine, which never quits by itself)
//! and `record_engine_ctl::switch_engine`, the call `Controller::apply_engine_target_as` makes.

use super::*;
use crate::record_dispatch::{mic_effects, pipe_setting_effects, refusal, Effect};
use crate::record_engine_ctl::switch_engine;
use std::sync::Barrier;
use std::time::Instant;

const SAVE: &str = r#"{"cmd":"save_clip","seconds":30}"#;
const MIC: &str = r#"{"cmd":"set_mic_mute","muted":true}"#;
const PAUSED: &str = "Recording is paused";
const SWITCHING: &str = "Switching games; try again in a moment";
const WAITING: &str = "Recall is waiting for a game; nothing is being recorded";

fn wait_for(deadline_secs: u64, mut f: impl FnMut() -> bool) -> bool {
    let deadline = Instant::now() + Duration::from_secs(deadline_secs);
    while Instant::now() < deadline {
        if f() {
            return true;
        }
        thread::sleep(Duration::from_millis(20));
    }
    f()
}

/// A running fake engine whose quit window lasts `grace_ms` (it is killed at its end).
fn running_fake_engine(grace_ms: u64) -> Supervisor {
    let engine = std::env::var_os("ComSpec").map(PathBuf::from).unwrap_or_else(|| PathBuf::from(r"C:\Windows\System32\cmd.exe"));
    let sup = Supervisor::start_with_locator(move || Some(engine), None);
    sup.shared.restart_grace_ms.store(grace_ms, Ordering::SeqCst);
    sup.retarget(Some(EngineSpec::default()));
    assert!(wait_for(5, || matches!(sup.health(), EngineHealth::Running { pid } if pid != 0)), "setup: the fake engine runs");
    sup
}

/// The first refused line while the old engine quits (it still runs).
fn refused_in_quit_window(sup: &Supervisor, line: &str) -> Sent {
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        let sent = sup.send_engine_line(line);
        if !sent.queued() {
            assert!(matches!(sup.health(), EngineHealth::Running { pid } if pid != 0), "refused while the old engine still runs");
            return sent;
        }
        thread::sleep(Duration::from_millis(5));
    }
    panic!("no line was refused in 5 s");
}

/// The line is refused while the old engine still runs (the window has not ended).
fn refused_now(sup: &Supervisor, line: &str) -> Sent {
    let sent = sup.send_engine_line(line);
    assert!(matches!(sup.health(), EngineHealth::Running { pid } if pid != 0), "setup: still in the quit window");
    sent
}

/// Pause (`apply_pause` -> `apply_engine_target_as(Off, Paused)`), then resume inside the
/// grace (`probe_foreground` -> `apply_engine_target(Window)`: a game switch).
#[test]
fn a_resume_inside_the_pause_grace_says_switching_games_not_paused() {
    let sup = running_fake_engine(3_000);
    switch_engine(&sup, None, Sent::Paused);
    assert_eq!(refused_in_quit_window(&sup, SAVE).reason(), Some(PAUSED), "setup: the pause's quit window");
    switch_engine(&sup, Some(EngineSpec::default()), Sent::Switching);
    assert_eq!(refused_now(&sup, SAVE).reason(), Some(SWITCHING), "a game is wanted again");
    sup.shutdown(Duration::from_millis(50));
}

/// A closed game's drain ends with no game (`retarget(None)`), then a game gets focus.
#[test]
fn a_game_after_a_drain_to_off_says_switching_games_not_waiting() {
    let sup = running_fake_engine(3_000);
    switch_engine(&sup, None, Sent::Switching);
    assert_eq!(refused_in_quit_window(&sup, SAVE).reason(), Some(WAITING), "setup: the stop's quit window");
    switch_engine(&sup, Some(EngineSpec::default()), Sent::Switching);
    assert_eq!(refused_now(&sup, SAVE).reason(), Some(SWITCHING), "a game is wanted again");
    sup.shutdown(Duration::from_millis(50));
}

/// `set_mic`, the pipe's `set_mode` and `set_config`'s live `set_source` lines skip the
/// pause gate (`capture_refusal`): their refusal must say why.
#[test]
fn mic_set_mode_and_set_source_refused_during_a_pause_say_recording_is_paused() {
    let sup = running_fake_engine(3_000);
    switch_engine(&sup, None, Sent::Paused);
    let mic = refused_in_quit_window(&sup, MIC);
    assert_eq!(refusal(&mic_effects(mic)), Some(PAUSED), "mic");
    let mode = refused_now(&sup, r#"{"cmd":"set_mode","value":"hdr"}"#);
    assert_eq!(pipe_setting_effects(mode), [Effect::Refused(PAUSED), Effect::ReplyError], "set_mode");
    let source = refused_now(&sup, r#"{"cmd":"set_source","index":0,"gain":0.5}"#);
    assert_eq!(pipe_setting_effects(source), [Effect::Refused(PAUSED), Effect::ReplyError], "set_source");
    sup.shutdown(Duration::from_millis(50));
}

/// A retarget landing between the manager reading the refusal and sending the quit is not
/// lost: the quit reads it under the same lock (`send_retarget_quit`). Two threads race the
/// quit and a retarget from one barrier, `RUNS` times. The stdin lock is held for a moment
/// at the start, which widens the gap between the two steps the old code took (read the
/// refusal, then quit) so the retarget lands in it often; every run must end with the
/// latest refusal.
#[test]
fn the_retarget_quit_takes_the_latest_refusal() {
    const RUNS: usize = 2_000;
    let shared = Shared::new(None);
    shared.attach_writer(Some(Box::new(std::io::sink())));
    let (start, done) = (Barrier::new(3), Barrier::new(3));
    let mut lost = 0;
    thread::scope(|s| {
        s.spawn(|| {
            for _ in 0..RUNS {
                start.wait();
                shared.send_retarget_quit();
                done.wait();
            }
        });
        s.spawn(|| {
            for _ in 0..RUNS {
                start.wait();
                shared.set_desired(Some(EngineSpec::default()), Sent::Restarting);
                done.wait();
            }
        });
        for _ in 0..RUNS {
            shared.child_stdin.lock().unwrap().quitting = None; // the next engine took over
            shared.set_desired(Some(EngineSpec::default()), Sent::Switching);
            let gen = shared.generation();
            let stdin = shared.child_stdin.lock().unwrap();
            start.wait();
            // Until the retarget is in `desired` (never, while the quit holds that lock), 200 us at most.
            let held = Instant::now();
            while held.elapsed() < Duration::from_micros(200) && shared.desired.try_lock().map_or(true, |g| g.0 == gen) {
                std::hint::spin_loop();
            }
            drop(stdin);
            done.wait();
            if shared.send_line(SAVE) != Sent::Restarting {
                lost += 1;
            }
        }
    });
    assert_eq!(lost, 0, "{lost} of {RUNS} runs lost the latest retarget's refusal");
}

/// Shutdown sets the stop flag, then quits with "isn't responding" under the stdin lock. A
/// retarget that read the flag before shutdown but takes the lock after must not replace it.
#[test]
fn a_retarget_racing_shutdown_keeps_isnt_responding() {
    let shared = Shared::new(None);
    shared.attach_writer(Some(Box::new(std::io::sink())));
    shared.send_quit(Sent::Switching); // a quit window is open
    thread::scope(|s| {
        let mut stdin = shared.child_stdin.lock().unwrap();
        let late = s.spawn(|| shared.set_desired(Some(EngineSpec::default()), Sent::Switching));
        while shared.desired.try_lock().is_ok() {
            thread::yield_now(); // until the retarget holds `desired` (it waits on stdin next)
        }
        thread::sleep(Duration::from_millis(50));
        shared.request_stop(Duration::ZERO);
        stdin.quitting = Some(Sent::NotResponding); // `finish_shutdown`'s `send_quit`
        drop(stdin);
        late.join().unwrap();
    });
    assert_eq!(shared.send_line(SAVE), Sent::NotResponding, "shutdown is no game switch");
}

/// The manager saw no target (`desired() == None`), then a game took focus before its
/// `end_quitting`: the game's "Switching games" stays until its engine spawns.
#[test]
fn a_game_landing_as_a_stop_window_ends_keeps_switching_games() {
    let shared = Shared::new(None);
    shared.attach_writer(Some(Box::new(std::io::sink())));
    shared.set_desired(None, Sent::Switching); // a drain ends with no game
    assert!(shared.send_retarget_quit());
    shared.attach_writer(None); // the old engine is gone
    let (gen, spec) = shared.desired();
    assert!(spec.is_none(), "setup: the manager sees Off");
    shared.set_desired(Some(EngineSpec::default()), Sent::Switching);
    shared.end_quitting(gen);
    assert_eq!(shared.send_line(SAVE), Sent::Switching, "a game is wanted, not 'isn't responding'");
    let (gen, _) = shared.desired();
    shared.set_desired(None, Sent::Switching);
    shared.end_quitting(gen + 1);
    assert_eq!(shared.send_line(SAVE), Sent::NotResponding, "a stop with no target since ends the window");
}

/// A resume with no game in front (`apply_pause(false)` -> `Supervisor::unpause`): the
/// lines that skip the pause gate say "waiting for a game", not "Recording is paused".
#[test]
fn mic_set_mode_and_set_source_after_a_resume_with_no_game_say_waiting() {
    let sup = running_fake_engine(3_000);
    switch_engine(&sup, None, Sent::Paused);
    let mic = refused_in_quit_window(&sup, MIC);
    assert_eq!(refusal(&mic_effects(mic)), Some(PAUSED), "setup: the pause's quit window");
    sup.unpause();
    assert_eq!(refusal(&mic_effects(refused_now(&sup, MIC))), Some(WAITING), "mic");
    let mode = refused_now(&sup, r#"{"cmd":"set_mode","value":"hdr"}"#);
    assert_eq!(pipe_setting_effects(mode), [Effect::Refused(WAITING), Effect::ReplyError], "set_mode");
    let source = refused_now(&sup, r#"{"cmd":"set_source","index":0,"gain":0.5}"#);
    assert_eq!(pipe_setting_effects(source), [Effect::Refused(WAITING), Effect::ReplyError], "set_source");
    sup.shutdown(Duration::from_millis(50));
}

/// A resume before the manager sent the pause's quit: the quit says "waiting" too.
#[test]
fn a_resume_before_the_pauses_quit_says_waiting() {
    let shared = Shared::new(None);
    shared.attach_writer(Some(Box::new(std::io::sink())));
    shared.set_desired(None, Sent::Paused);
    shared.unpause();
    assert!(shared.send_retarget_quit());
    assert_eq!(shared.send_line(SAVE), Sent::Waiting);
}
