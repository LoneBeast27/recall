//! design-audio-mute P1/P14 and the R1 conditions (merge gate): an engine file without the
//! protocol marker next to this controller gets no input and no picked device, keeps its outputs
//! and game audio, and the user is told. Fake engine files in a temp dir (a drop guard removes
//! it, a panic included); no engine runs.

use std::path::PathBuf;
use std::sync::mpsc::channel;

use serde_json::json;

use super::super::Shared;
use super::*;
use crate::engine_policy::EngineSpec;

/// A temp dir removed when the test ends, a panic included.
struct TempDir(PathBuf);

impl TempDir {
    fn new(tag: &str) -> TempDir {
        let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
        let dir = std::env::temp_dir().join(format!("recall-hdr-floor-{tag}-{}-{nanos}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        TempDir(dir)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

const LIST: &str = r#"[
  {"id":"game","kind":"game","name":"Game"},
  {"id":"mic","kind":"input","name":"Mic"},
  {"id":"line","kind":"input","name":"Line","device":"Line (Box)","mic_mute":false},
  {"id":"out","kind":"output","name":"Out"},
  {"id":"pod","kind":"output","name":"Pod","device":"Speakers (Pod)"},
  {"id":"voice","kind":"app","name":"Voice","exe":"chat.exe"}
]"#;

fn spec() -> EngineSpec {
    EngineSpec { env: vec![(SOURCES_ENV.into(), Some(LIST.into()))] }
}

fn ids(value: &str) -> Vec<String> {
    crate::audio_intent::parse(value).unwrap().into_iter().map(|s| s.id).collect()
}

fn env_list(spec: &EngineSpec) -> String {
    spec.env.iter().find(|(k, _)| k == SOURCES_ENV).and_then(|(_, v)| v.clone()).unwrap()
}

/// The coordinator's R1 test: an old engine (no marker) beside the new controller.
#[test]
fn an_engine_file_without_the_marker_gets_no_input_or_picked_device_and_the_user_is_told() {
    let dir = TempDir::new("old");
    let old = dir.0.join("recall.exe");
    std::fs::write(&old, b"MZ an engine built before the sync handshake").unwrap();
    let shared = Shared::new(None);
    let (tx, rx) = channel();
    *shared.events.lock().unwrap() = Some(tx);
    shared.spawn_sources.set(LIST);
    let read = shared.spawn_sources.apply(Some(spec())).unwrap();
    let spawned = shared.floor_spec(&old, &read);
    assert_eq!(ids(&env_list(&spawned)), vec!["game", "out", "voice"], "no input, no picked device; game, default output and app stay");
    assert!(shared.spawn_sources.below_floor());
    assert!(spawned.env.iter().any(|(k, v)| k == "RECALL_AUDIO_MIC_OFF" && v.as_deref() == Some("1")), "a very old engine opens no default mic");
    assert_eq!(rx.try_recv().ok(), Some(json!({"type": ENGINE_OUTDATED})), "the notice is raised");
    // Its restatement carries no input or picked device either, and the shape matches (synced).
    let (baked, current) = shared.spawn_sources.baked_and_current();
    assert!(crate::audio_intent::same_shape(&baked, &current), "no respawn loop");
    let restated = crate::audio_intent::restatement(&baked, &current);
    assert!(restated.iter().all(|l| !matches!(l["id"].as_str(), Some("mic" | "line" | "pod"))), "{restated:?}");
}

#[test]
fn an_engine_file_with_the_marker_gets_the_whole_list() {
    let dir = TempDir::new("new");
    let new = dir.0.join("recall.exe");
    let mut bytes = b"MZ ...".to_vec();
    bytes.extend_from_slice(PROTOCOL_MARKER);
    bytes.extend_from_slice(b"... rest");
    std::fs::write(&new, bytes).unwrap();
    assert!(has_marker(&new));
    let shared = Shared::new(None);
    let spawned = shared.floor_spec(&new, &spec());
    assert_eq!(env_list(&spawned), LIST);
    assert!(!shared.spawn_sources.below_floor());
    assert!(!has_marker(&dir.0.join("missing.exe")), "an unreadable file has none");
    let manager = include_str!("supervisor_manager.rs").split_whitespace().collect::<Vec<_>>().join(" ");
    assert!(manager.contains("let spec = &shared.floor_spec(path, spec);"), "every spawn goes through it");
}

/// The game captured by its process tree keeps its track even with a named desktop device.
#[test]
fn a_process_loopback_game_keeps_its_track() {
    let game: Vec<AudioSource> = crate::audio_intent::parse(r#"[{"id":"game","kind":"game","name":"Game","device":"Speakers (Pod)"}]"#).unwrap();
    assert_eq!(withhold_devices(&game, true).len(), 1);
    assert!(withhold_devices(&game, false).is_empty(), "loopback of a picked device: withheld");
}

/// P14: a hello without sync_hold although the file had the marker: every later spawn is below
/// the floor and this engine is restarted, once; the user is told.
#[test]
fn a_hello_without_sync_hold_restarts_inputs_off() {
    let shared = Shared::new(None);
    let (tx, rx) = channel();
    *shared.events.lock().unwrap() = Some(tx);
    shared.set_desired(Some(spec()), crate::record_dispatch::Sent::Waiting);
    let gen = shared.generation();
    shared.on_hello_floor(&json!({"type": "hello", "sync_hold": true}));
    assert_eq!(shared.generation(), gen, "a sync-hold engine: nothing");
    shared.on_hello_floor(&json!({"type": "hello", "live_sources": true}));
    assert_eq!(shared.generation(), gen + 1, "restarted");
    assert!(shared.spawn_sources.forced_below());
    assert_eq!(rx.try_recv().ok(), Some(json!({"type": ENGINE_OUTDATED})));
    let dir = TempDir::new("forced");
    let new = dir.0.join("recall.exe");
    std::fs::write(&new, PROTOCOL_MARKER).unwrap();
    assert_eq!(ids(&env_list(&shared.floor_spec(&new, &spec()))), vec!["game", "out", "voice"], "the respawn withholds them");
    shared.on_hello_floor(&json!({"type": "hello"}));
    assert_eq!(shared.generation(), gen + 1, "once: it was already given no device");
    let protocol = include_str!("supervisor_protocol.rs").split_whitespace().collect::<Vec<_>>().join(" ");
    assert!(protocol.contains("shared.on_hello_floor(&val);"), "the hello path checks it");
}

/// The controller's marker is the engine's.
#[test]
fn the_marker_matches_the_engine() {
    let engine = include_str!("../../recall/src/engine_stdin.rs");
    assert!(engine.contains(std::str::from_utf8(PROTOCOL_MARKER).unwrap()));
}

/// P2: every spawn tells the engine a `sync` will come (its env), the same name it reads.
#[test]
fn every_spawn_sets_the_sync_env() {
    let cmd = super::super::locate::engine_command(std::path::Path::new("recall.exe"), &spec(), None);
    assert!(cmd.get_envs().any(|(k, v)| k.to_str() == Some(super::super::locate::STDIN_SYNC_ENV) && v.and_then(|v| v.to_str()) == Some("1")));
    let engine = include_str!("../../recall/src/engine_stdin.rs");
    assert!(engine.contains("pub(crate) const SYNC_ENV: &str = \"RECALL_STDIN_SYNC\";"));
}

/// N4: one failed read of the engine file is not cached as "no marker": the next spawn reads
/// it again and gets the full list.
#[test]
fn a_failed_read_is_retried_at_the_next_spawn() {
    let dir = TempDir::new("retry");
    let exe = dir.0.join("recall.exe");
    std::fs::write(&exe, PROTOCOL_MARKER).unwrap();
    assert!(!has_marker_with(&exe, |_| Err(std::io::Error::other("sharing violation"))), "this spawn: below the floor");
    assert!(has_marker_with(&exe, std::fs::read), "the next spawn reads it again");
    assert!(!has_marker_with(&dir.0.join("none.exe"), std::fs::read));
}
