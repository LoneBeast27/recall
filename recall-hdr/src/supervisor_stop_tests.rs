//! The quit window's end (merge-gate P2 on the save-during-switch work). A stop to Off (a
//! closed game's drain ends with no game, a pause) and a respawn that fails each end the
//! quit window: the `quitting` flag is cleared there, not only when a next engine's stdin
//! attaches. A line sent while the engine quits for a stop to Off says "waiting for a
//! game" (the controller's `capture_refusal` text for no target), never "Switching games".

use super::quit_tests::Recorded;
use super::*;
use crate::record_engine::{capture_refusal, EngineTarget};
use std::time::Instant;

const SAVE: &str = r#"{"cmd":"save_clip","seconds":30}"#;

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

/// The fake engine (cmd.exe never quits by itself: the quit window lasts its grace) runs;
/// the recorder stands in for its stdin.
fn running_fake_engine() -> (Supervisor, Recorded) {
    let engine = std::env::var_os("ComSpec").map(PathBuf::from).unwrap_or_else(|| PathBuf::from(r"C:\Windows\System32\cmd.exe"));
    let sup = Supervisor::start_with_locator(move || Some(engine), None);
    sup.shared.restart_grace_ms.store(400, Ordering::SeqCst);
    sup.retarget(Some(EngineSpec::default()));
    assert!(wait_for(5, || matches!(sup.health(), EngineHealth::Running { pid } if pid != 0)), "setup: the fake engine runs");
    let rec = Recorded::attach(&sup.shared);
    (sup, rec)
}

fn quitting(sup: &Supervisor) -> Option<Sent> {
    sup.shared.child_stdin.lock().unwrap().quitting
}

/// The first refused save while the old engine quits (it still runs).
fn refusal_in_quit_window(sup: &Supervisor) -> Sent {
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        let sent = sup.send_engine_line(SAVE);
        if !sent.queued() {
            assert!(matches!(sup.health(), EngineHealth::Running { pid } if pid != 0), "refused while the old engine still runs");
            return sent;
        }
        thread::sleep(Duration::from_millis(5));
    }
    panic!("no save was refused in 5 s");
}

/// A drain that ends with no game, and a pause, both apply `Off`: `retarget(None)`
/// (`Controller::apply_engine_target(&EngineTarget::Off)`, record_actions.rs).
#[test]
fn a_save_while_the_engine_quits_for_a_stop_to_off_says_waiting_for_a_game() {
    let (sup, rec) = running_fake_engine();
    sup.retarget(None);
    let refused = refusal_in_quit_window(&sup);
    let waiting = capture_refusal(false, false, &EngineTarget::Off);
    assert_eq!(refused.reason(), waiting, "no game comes next: not 'Switching games'");
    assert!(wait_for(5, || sup.health() == EngineHealth::Stopped), "the engine stopped");
    assert_eq!(super::quit_tests::after_quit(&rec.lines()), Vec::<String>::new(), "a save went behind the quit");
    sup.shutdown(Duration::from_millis(50));
}

/// A restart that is no game switch, to Off (a recovery during a drain that ends in Off):
/// no engine comes next either.
#[test]
fn a_restart_to_off_says_waiting_for_a_game() {
    let (sup, _rec) = running_fake_engine();
    sup.restart(None, false);
    let refused = refusal_in_quit_window(&sup);
    assert_eq!(refused.reason(), capture_refusal(false, false, &EngineTarget::Off));
    sup.shutdown(Duration::from_millis(50));
}

#[test]
fn a_stop_to_off_clears_the_quitting_flag() {
    let (sup, _rec) = running_fake_engine();
    sup.retarget(None);
    assert!(wait_for(5, || sup.health() == EngineHealth::Stopped), "the engine stopped");
    assert!(wait_for(2, || quitting(&sup).is_none()), "still quitting after the stop to Off: {:?}", quitting(&sup));
    assert_eq!(sup.send_engine_line(SAVE), Sent::NotResponding, "no engine: not 'Switching games'");
    sup.shutdown(Duration::from_millis(50));
}

/// A next engine that cannot be spawned (an env entry the OS refuses) leaves no engine.
#[test]
fn a_failed_respawn_clears_the_quitting_flag() {
    let (sup, _rec) = running_fake_engine();
    let bad = EngineSpec { env: vec![("RECALL\0BAD".into(), Some("x".into()))] };
    sup.retarget(Some(bad));
    assert_eq!(refusal_in_quit_window(&sup), Sent::Switching, "setup: the quit window of a game switch");
    assert!(wait_for(5, || matches!(sup.health(), EngineHealth::Restarting { .. } | EngineHealth::GivenUp)), "the respawn failed");
    assert!(wait_for(2, || quitting(&sup).is_none()), "still quitting after the failed respawn: {:?}", quitting(&sup));
    assert_eq!(sup.send_engine_line(SAVE), Sent::NotResponding, "no engine came up: not 'Switching games'");
    sup.shutdown(Duration::from_millis(50));
}
