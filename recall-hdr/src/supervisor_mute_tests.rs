//! The attach state (design-audio-mute P2, P8, P9, P19; the cut-down's coalescing state slot): a
//! spawned engine's first lines are its whole audio state, then `sync`; an Alt+M or an
//! Audio-page change between the spawn's env and the attach is never lost. A recorder stands in
//! for the engine's stdin.

use serde_json::{json, Value};

use super::super::quit_tests::Recorded;
use super::super::Shared;
use super::mute_line;
use crate::engine_policy::EngineSpec;
use crate::record_audio_sources::SOURCES_ENV;
use crate::record_dispatch::Sent;

const SAVE: &str = r#"{"cmd":"save_clip","seconds":30}"#;
const LINE_IN: &str = r#"[{"id":"line","kind":"input","name":"Line","device":"Line (Box)","mic_mute":false}]"#;
const LINE_IN_MUTED: &str = r#"[{"id":"line","kind":"input","name":"Line","device":"Line (Box)"}]"#;
const OTHER_DEVICE: &str = r#"[{"id":"line","kind":"input","name":"Line","device":"Line (Other)","mic_mute":false}]"#;

fn json_lines(rec: &Recorded) -> Vec<Value> {
    rec.raw_lines().iter().map(|l| serde_json::from_str(l).unwrap()).collect()
}

/// The manager's spec read for a spawn whose env gets `sources` (`Shared::desired`).
fn bake(shared: &Shared, sources: &str) {
    shared.spawn_sources.set(sources);
    shared.spawn_sources.apply(Some(EngineSpec { env: vec![(SOURCES_ENV.into(), None)] }));
}

#[test]
fn an_alt_m_between_spawn_and_attach_is_restated_before_sync() {
    let shared = Shared::new(None);
    shared.set_mute_and_send(false); // live mic, no engine yet
    let baked = shared.spawn_mute.get(); // spawn_engine: the env gets "live"
    // Alt+M after the env was read, before the stdin is attached: no queue takes the press.
    assert_eq!(shared.set_mute_and_send(true), Sent::NotResponding);
    let rec = Recorded::attach_spawned(&shared, baked);
    std::thread::sleep(std::time::Duration::from_millis(50)); // the state is written first
    assert_eq!(shared.send_line(SAVE), Sent::Queued);
    shared.attach_writer(None);
    let lines = json_lines(&rec);
    // Not a press (review R2-audio-r9-3): it was made before any stream of this engine opened.
    assert_eq!(lines[0], json!({"cmd": "set_mic_mute", "muted": true}), "{lines:?}");
    assert_eq!(lines[1], json!({"cmd": "sync"}), "{lines:?}");
    assert_eq!(lines[2]["cmd"], "save_clip");
}

/// P8 (083a6d7 changed): the mute is restated even when the env already had it.
#[test]
fn a_spawn_with_the_current_mute_is_still_restated() {
    let shared = Shared::new(None);
    shared.set_mute_and_send(true);
    let rec = Recorded::attach_spawned(&shared, shared.spawn_mute.get());
    shared.attach_writer(None);
    assert_eq!(json_lines(&rec), vec![json!({"cmd": "set_mic_mute", "muted": true}), json!({"cmd": "sync"})]);
}

/// P9: the user's Alt+M after the attach is a press, and the state follows it.
#[test]
fn an_alt_m_after_the_attach_is_a_press_then_the_state() {
    let shared = Shared::new(None);
    shared.set_mute_and_send(false);
    let rec = Recorded::attach_spawned(&shared, shared.spawn_mute.get());
    std::thread::sleep(std::time::Duration::from_millis(50));
    assert_eq!(shared.set_mute_and_send(true), Sent::Queued);
    shared.attach_writer(None);
    let lines = json_lines(&rec);
    let press = lines.iter().position(|l| *l == json!({"cmd": "set_mic_mute", "muted": true, "press": true})).expect("the press");
    assert!(lines[press + 1..].contains(&json!({"cmd": "set_mic_mute", "muted": true})), "the state after it: {lines:?}");
    // The user's Alt+M is a press (gate review P2-b; guarded by review R2-audio-r9-4).
    assert_eq!(mute_line(true), crate::record_dispatch::mic_line(true, true));
}

/// P8: mute, every source's state (the effective flag), then `sync`, in that order.
#[test]
fn attach_restates_then_syncs() {
    let shared = Shared::new(None);
    shared.set_mute_and_send(true);
    bake(&shared, LINE_IN);
    let rec = Recorded::attach_spawned(&shared, shared.spawn_mute.get());
    shared.attach_writer(None);
    let lines = json_lines(&rec);
    assert_eq!(lines.len(), 3, "{lines:?}");
    assert_eq!(lines[0]["cmd"], "set_mic_mute");
    assert_eq!((lines[1]["cmd"].as_str(), lines[1]["id"].as_str(), lines[1]["mic_mute"].as_bool()), (Some("set_source"), Some("line"), Some(false)));
    assert_eq!(lines[2], json!({"cmd": "sync"}));
}

/// P2/P10: an Audio-page change racing the spawn lands in the env, or in the first state, or
/// in a later state; never nowhere. Here: after the bake, before the attach, then after it.
#[test]
fn the_baked_state_and_a_change_are_one_step() {
    let shared = Shared::new(None);
    bake(&shared, LINE_IN);
    let sup = super::super::Supervisor { shared: std::sync::Arc::new(shared), handle: None };
    // The line-in's "Alt+M mutes it" is ticked while the pid is still 0 (no queue).
    assert_eq!(sup.update_audio(Some(LINE_IN_MUTED)), Sent::NotResponding);
    let rec = Recorded::attach_spawned(&sup.shared, None);
    std::thread::sleep(std::time::Duration::from_millis(50));
    assert_eq!(sup.update_audio(Some(LINE_IN)), Sent::Queued); // and back after the attach
    sup.shared.attach_writer(None);
    let lines = json_lines(&rec);
    assert_eq!(lines[0]["mic_mute"], json!(true), "the first state has the change: {lines:?}");
    assert_eq!(lines[1], json!({"cmd": "sync"}));
    assert_eq!(lines.last(), Some(&json!({"cmd": "sync"})), "the later state ends with sync too");
    assert!(lines.iter().any(|l| l["mic_mute"] == json!(false)), "the later one follows it: {lines:?}");
}

/// B5: a change that moved the track shape during the spawn: the state retires the old track
/// and sends no `sync` (that engine stays held), and the manager respawns it once.
#[test]
fn a_shape_move_during_spawn_sends_no_sync_and_respawns() {
    let shared = Shared::new(None);
    shared.set_desired(Some(EngineSpec { env: vec![(SOURCES_ENV.into(), Some(LINE_IN.into()))] }), Sent::Waiting);
    let (gen, _) = shared.desired(); // the manager reads the spec (bakes LINE_IN)
    shared.spawn_sources.set(OTHER_DEVICE); // re-picked under another name, pid still 0
    let rec = Recorded::attach_spawned(&shared, None);
    assert!(shared.respawn_if_sources_moved(gen), "the manager respawns it");
    shared.attach_writer(None);
    let lines = json_lines(&rec);
    assert!(lines.iter().all(|l| l["cmd"] != "sync"), "no sync: {lines:?}");
    assert!(lines.iter().any(|l| l["retire"] == json!(true) && l["id"] == "line"), "the old device is retired: {lines:?}");
    let _ = shared.desired(); // the respawn reads the spec: baked on the current list
    assert!(!shared.respawn_if_sources_moved(gen + 1), "once");
}

/// N3: the shape moves before the attach (no sync) and moves back before the pid shows. The
/// engine that got no sync must still be respawned, or every track stays held all run.
#[test]
fn a_withheld_sync_respawns_even_when_the_shape_moves_back() {
    let shared = Shared::new(None);
    shared.set_desired(Some(EngineSpec { env: vec![(SOURCES_ENV.into(), Some(LINE_IN.into()))] }), Sent::Waiting);
    let (gen, _) = shared.desired();
    shared.spawn_sources.set(OTHER_DEVICE);
    let rec = Recorded::attach_spawned(&shared, None); // withholds sync
    shared.spawn_sources.set(LINE_IN); // and back, before the pid
    assert!(shared.respawn_if_sources_moved(gen), "respawned on the withheld sync");
    shared.attach_writer(None);
    assert!(rec.raw_lines().iter().all(|l| !l.contains(r#""sync""#)), "that engine got no sync");
    let src = include_str!("supervisor_sources.rs").split_whitespace().collect::<Vec<_>>().join(" ");
    assert!(src.contains("s.withheld = !same;"), "the attach records it");
}

/// P19 and the cut-down: an Audio-page change for an engine told to quit is not refused to it:
/// its state goes ahead of the `quit` (the caller is still told why).
#[test]
fn a_change_after_quit_goes_ahead_of_the_quit() {
    let shared = Shared::new(None);
    let rec = Recorded::attach(&shared);
    let sup = super::super::Supervisor { shared: std::sync::Arc::new(shared), handle: None };
    assert!(sup.shared.send_quit(Sent::Switching));
    assert_eq!(sup.update_audio(Some(LINE_IN_MUTED)), Sent::Switching, "the caller is told the engine is ending");
    sup.shared.attach_writer(None);
    let lines = rec.raw_lines();
    assert!(super::super::quit_tests::after_quit(&lines).is_empty(), "nothing after the quit: {lines:?}");
}
