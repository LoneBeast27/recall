//! The quit window's refusal follows the latest target (merge-gate P2 on the save stack):
//! a pause then a resume inside the grace, or a drain to Off then a game taking focus, says
//! "Switching games" (a game is wanted again); a pause's own window says "Recording is
//! paused" for the lines that skip the controller's pause gate (mic, set_mode, set_source).
//! Driven through the real manager (cmd.exe as the fake engine, which never quits by itself)
//! and `record_engine_ctl::switch_engine`, the call `Controller::apply_engine_target_as` makes.

use super::*;
use crate::record_dispatch::{mic_effects, pipe_setting_effects, refusal, Effect};
use crate::record_engine_ctl::switch_engine;
use std::time::Instant;

const SAVE: &str = r#"{"cmd":"save_clip","seconds":30}"#;
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
    let mic = refused_in_quit_window(&sup, r#"{"cmd":"set_mic_mute","muted":true}"#);
    assert_eq!(refusal(&mic_effects(mic)), Some(PAUSED), "mic");
    let mode = refused_now(&sup, r#"{"cmd":"set_mode","value":"hdr"}"#);
    assert_eq!(pipe_setting_effects(mode), [Effect::Refused(PAUSED), Effect::ReplyError], "set_mode");
    let source = refused_now(&sup, r#"{"cmd":"set_source","index":0,"gain":0.5}"#);
    assert_eq!(pipe_setting_effects(source), [Effect::Refused(PAUSED), Effect::ReplyError], "set_source");
    sup.shutdown(Duration::from_millis(50));
}

/// A retarget landing between the manager reading the refusal and sending the quit is not
/// lost: the quit reads it under the same lock (`send_retarget_quit`).
#[test]
fn the_retarget_quit_takes_the_latest_refusal() {
    let shared = Shared::new(None);
    shared.attach_writer(Some(Box::new(std::io::sink())));
    shared.set_desired(None, Sent::Paused);
    shared.set_desired(Some(EngineSpec::default()), Sent::Restarting);
    assert!(shared.send_retarget_quit());
    assert_eq!(shared.send_line(SAVE), Sent::Restarting);
}
