//! An Alt+M between a spawn's env read and its stdin attach (P1-a): the engine started with
//! the old mute and the line had no queue to go to, so it must be the engine's first line.

use super::super::quit_tests::Recorded;
use super::super::Shared;
use super::mute_line;
use crate::record_dispatch::Sent;

const SAVE: &str = r#"{"cmd":"save_clip","seconds":30}"#;

#[test]
fn an_alt_m_between_spawn_and_attach_is_the_engines_first_line() {
    let shared = Shared::new(None);
    shared.set_mute_and_send(false); // live mic, no engine yet
    let baked = shared.spawn_mute.get(); // spawn_engine: the env gets "live"
    // Alt+M after the env was read, before the stdin is attached: no queue takes the line.
    assert_eq!(shared.set_mute_and_send(true), Sent::NotResponding);
    let rec = Recorded::attach_spawned(&shared, baked);
    assert_eq!(shared.send_line(SAVE), Sent::Queued);
    shared.attach_writer(None);
    let lines = rec.lines();
    // Not a press (review R2-audio-r9-3): it was made before any stream of this engine opened.
    let catch_up = crate::record_dispatch::mic_line(true, false);
    assert_eq!(lines.first(), Some(&catch_up), "the engine runs with live mic: {lines:?}");
    assert!(!catch_up.contains("press"), "{catch_up}");
    assert_eq!(lines.len(), 2, "{lines:?}");
}

#[test]
fn a_spawn_with_the_current_mute_gets_no_extra_line() {
    let shared = Shared::new(None);
    shared.set_mute_and_send(true);
    let rec = Recorded::attach_spawned(&shared, shared.spawn_mute.get());
    assert_eq!(shared.send_line(SAVE), Sent::Queued);
    shared.attach_writer(None);
    assert_eq!(rec.lines(), vec![SAVE.to_string()]);
}

#[test]
fn an_alt_m_after_the_attach_is_queued_directly() {
    let shared = Shared::new(None);
    shared.set_mute_and_send(false);
    let rec = Recorded::attach_spawned(&shared, shared.spawn_mute.get());
    assert_eq!(shared.set_mute_and_send(true), Sent::Queued);
    shared.attach_writer(None);
    assert_eq!(rec.lines(), vec![mute_line(true)]);
}
