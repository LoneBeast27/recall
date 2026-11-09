//! The attach restatement (design-audio-mute P2, P8, P9, P19): a spawned engine's first lines are
//! the whole audio intent, then `sync`; an Alt+M or an Audio-page change between the spawn's env
//! and the attach is never lost. A recorder stands in for the engine's stdin.

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

/// A line as JSON without its sequence number (asserted apart).
fn unseq(line: &str) -> Value {
    let mut v: Value = serde_json::from_str(line).unwrap();
    assert!(v.get("seq").and_then(Value::as_u64).is_some(), "every audio line has a seq: {line}");
    v.as_object_mut().unwrap().remove("seq");
    v
}

/// The lines after the restatement's opening `restate` (F-1), which every attach sends first.
fn restated(rec: &Recorded) -> Vec<String> {
    let mut lines = rec.raw_lines();
    assert!(lines.first().is_some_and(|l| l.contains(r#""cmd":"restate""#)), "{lines:?}");
    lines.remove(0);
    lines
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
    // Alt+M after the env was read, before the stdin is attached: no queue takes the line.
    assert_eq!(shared.set_mute_and_send(true), Sent::NotResponding);
    let rec = Recorded::attach_spawned(&shared, baked);
    assert_eq!(shared.send_line(SAVE), Sent::Queued);
    shared.attach_writer(None);
    let lines = restated(&rec);
    // Not a press (review R2-audio-r9-3): it was made before any stream of this engine opened.
    assert_eq!(unseq(&lines[0]), json!({"cmd": "set_mic_mute", "muted": true}), "{lines:?}");
    assert_eq!(unseq(&lines[1]), json!({"cmd": "sync"}), "{lines:?}");
    assert_eq!(lines[2], SAVE);
}

/// P8 (083a6d7 changed): the mute is restated even when the env already had it.
#[test]
fn a_spawn_with_the_current_mute_is_still_restated() {
    let shared = Shared::new(None);
    shared.set_mute_and_send(true);
    let rec = Recorded::attach_spawned(&shared, shared.spawn_mute.get());
    shared.attach_writer(None);
    let lines: Vec<Value> = restated(&rec).iter().map(|l| unseq(l)).collect();
    assert_eq!(lines, vec![json!({"cmd": "set_mic_mute", "muted": true}), json!({"cmd": "sync"})]);
}

/// P9: the user's Alt+M after the attach is a press, with its seq.
#[test]
fn an_alt_m_after_the_attach_is_queued_directly() {
    let shared = Shared::new(None);
    shared.set_mute_and_send(false);
    let rec = Recorded::attach_spawned(&shared, shared.spawn_mute.get());
    assert_eq!(shared.set_mute_and_send(true), Sent::Queued);
    shared.attach_writer(None);
    let lines = restated(&rec);
    assert_eq!(unseq(lines.last().unwrap()), json!({"cmd": "set_mic_mute", "muted": true, "press": true}));
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
    let lines: Vec<Value> = restated(&rec).iter().map(|l| unseq(l)).collect();
    assert_eq!(lines.len(), 3, "{lines:?}");
    assert_eq!(lines[0]["cmd"], "set_mic_mute");
    assert_eq!((lines[1]["cmd"].as_str(), lines[1]["id"].as_str(), lines[1]["mic_mute"].as_bool()), (Some("set_source"), Some("line"), Some(false)));
    assert_eq!(lines[2], json!({"cmd": "sync"}));
}

/// P2/P10: an Audio-page change racing the spawn lands in the env, or in the restatement, or
/// as a live line after it; never nowhere. Here: after the bake, before the attach.
#[test]
fn the_baked_state_and_a_change_are_one_step() {
    let shared = Shared::new(None);
    bake(&shared, LINE_IN);
    let sup = super::super::Supervisor { shared: std::sync::Arc::new(shared), handle: None };
    // The line-in's "Alt+M mutes it" is ticked while the pid is still 0 (no queue).
    sup.update_audio(Some(LINE_IN_MUTED), &[json!({"cmd": "set_source", "id": "line", "mic_mute": true})]);
    let rec = Recorded::attach_spawned(&sup.shared, None);
    // And one after the attach goes live, after the restatement.
    assert_eq!(sup.update_audio(Some(LINE_IN), &[json!({"cmd": "set_source", "id": "line", "mic_mute": false})]), Sent::Queued);
    sup.shared.attach_writer(None);
    let lines: Vec<Value> = restated(&rec).iter().map(|l| unseq(l)).collect();
    assert_eq!(lines[0]["mic_mute"], json!(true), "the restatement has the change: {lines:?}");
    assert_eq!(lines[1], json!({"cmd": "sync"}));
    assert_eq!(lines[2]["mic_mute"], json!(false), "the later one follows it");
}

/// B5: a change that moved the track shape during the spawn: the restatement retires the old
/// track and sends no `sync` (that engine stays held), and the manager respawns it once.
#[test]
fn a_shape_move_during_spawn_sends_no_sync_and_respawns() {
    let shared = Shared::new(None);
    shared.set_desired(Some(EngineSpec { env: vec![(SOURCES_ENV.into(), Some(LINE_IN.into()))] }), Sent::Waiting);
    let (gen, _) = shared.desired(); // the manager reads the spec (bakes LINE_IN)
    shared.spawn_sources.set(OTHER_DEVICE); // re-picked under another name, pid still 0
    let rec = Recorded::attach_spawned(&shared, None);
    assert!(shared.respawn_if_sources_moved(gen), "the manager respawns it");
    shared.attach_writer(None);
    let lines: Vec<Value> = restated(&rec).iter().map(|l| unseq(l)).collect();
    assert!(lines.iter().all(|l| l["cmd"] != "sync"), "no sync: {lines:?}");
    assert!(lines.iter().any(|l| l["retire"] == json!(true) && l["id"] == "line"), "the old device is retired: {lines:?}");
    let _ = shared.desired(); // the respawn reads the spec: baked on the current list
    assert!(!shared.respawn_if_sources_moved(gen + 1), "once");
}

/// P19: an engine told to quit gets no restatement and no `sync`, and no live change.
#[test]
fn no_sync_after_quit() {
    let shared = Shared::new(None);
    let rec = Recorded::attach(&shared);
    assert!(shared.send_quit(Sent::Switching));
    let sup = super::super::Supervisor { shared: std::sync::Arc::new(shared), handle: None };
    assert!(!sup.restate_audio(), "nothing restated to a quitting engine");
    assert_eq!(sup.update_audio(Some(LINE_IN), &[json!({"cmd": "set_source", "id": "line", "retire": true})]), Sent::Switching);
    sup.shared.attach_writer(None);
    let lines = restated(&rec);
    assert!(super::super::quit_tests::after_quit(&lines).is_empty(), "{lines:?}");
}

/// N3: the shape moves before the attach (no sync) and moves back before the pid shows. The
/// engine that got no sync must still be respawned, or every device track stays held all run.
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
    assert!(restated(&rec).iter().all(|l| !l.contains("\"sync\"")), "that engine got no sync");
    let src = include_str!("supervisor_sources.rs").split_whitespace().collect::<Vec<_>>().join(" ");
    assert!(src.contains("self.spawn_sources.lock().withheld = !same;"), "the attach records it");
}

/// F-1: a line the full queue drops is not counted as sent, leaves the intent dirty, and a
/// restatement stops at its first dropped line (no `sync` after a drop).
#[test]
fn a_dropped_line_is_never_counted_and_a_restatement_stops_at_a_drop() {
    let shared = Shared::new(None);
    super::super::tests::wedge_and_fill(&shared); // the engine reads nothing; the queue is full
    let sent = shared.spawn_sources.last_sent();
    assert!(!shared.set_mute_and_send(true).queued(), "dropped");
    assert_eq!(shared.spawn_sources.last_sent(), sent, "a dropped line is not sent");
    assert!(shared.spawn_sources.dirty(), "the intent is dirty until a whole restatement");
    let mut offered = Vec::new();
    let lines: Vec<String> = ["restate", "set_mic_mute", "sync"].iter().map(|c| format!("{{\"cmd\":\"{c}\"}}")).collect();
    assert!(!super::super::sources::queue_all(&lines, |l| {
        offered.push(l.to_owned());
        false
    }));
    assert_eq!(offered.len(), 1, "stopped at the first drop: no sync after it");
    let mute = include_str!("supervisor_mute.rs").split_whitespace().collect::<Vec<_>>().join(" ");
    assert!(mute.contains("self.restate_if_dirty(&g);"), "a drop re-queues the whole intent");
}

/// F-1: every restatement starts with `restate` (the engine's ack baseline).
#[test]
fn a_restatement_starts_with_restate() {
    let shared = Shared::new(None);
    let rec = Recorded::attach_spawned(&shared, None);
    shared.attach_writer(None);
    let lines = rec.raw_lines();
    assert_eq!(unseq(&lines[0]), json!({"cmd": "restate"}), "{lines:?}");
}
