//! design-audio-mute P1/P14 and the R1 conditions (merge gate): an engine without
//! `sync_hold` in its hello, next to this controller, gets no input and no picked device, keeps
//! its outputs and game audio, and the user is told. No engine runs.

use std::path::Path;
use std::sync::mpsc::channel;

use serde_json::json;

use super::super::Shared;
use super::*;
use crate::engine_policy::EngineSpec;

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
    assert_eq!(ids(&env_list(&shared.floor_spec(Path::new("recall.exe"), &spec()))), vec!["game", "out", "voice"], "the respawn withholds them");
    shared.on_hello_floor(&json!({"type": "hello"}));
    assert_eq!(shared.generation(), gen + 1, "once: it was already given no device");
    let protocol = include_str!("supervisor_protocol.rs").split_whitespace().collect::<Vec<_>>().join(" ");
    assert!(protocol.contains("shared.on_hello_floor(&val);"), "the hello path checks it");
}

/// P2: every spawn tells the engine a `sync` will come (its env), the same name it reads.
#[test]
fn every_spawn_sets_the_sync_env() {
    let cmd = super::super::locate::engine_command(std::path::Path::new("recall.exe"), &spec(), None);
    assert!(cmd.get_envs().any(|(k, v)| k.to_str() == Some(super::super::locate::STDIN_SYNC_ENV) && v.and_then(|v| v.to_str()) == Some("1")));
    let engine = include_str!("../../recall/src/engine_stdin.rs");
    assert!(engine.contains("pub(crate) const SYNC_ENV: &str = \"RECALL_STDIN_SYNC\";"));
}

/// The R1 conditions (merge gate) through the hello check: an old engine (no `sync_hold`)
/// beside this controller: the next spawn gets no input or picked device, keeps its outputs and
/// game audio (and RECALL_AUDIO_MIC_OFF for a very old one), and the notice is raised; an
/// engine that says `sync_hold` gets the whole list.
#[test]
fn an_old_engine_gets_no_input_or_picked_device_and_the_user_is_told() {
    let shared = Shared::new(None);
    let (tx, rx) = channel();
    *shared.events.lock().unwrap() = Some(tx);
    shared.spawn_sources.set(LIST);
    let read = shared.spawn_sources.apply(Some(spec())).unwrap();
    assert_eq!(env_list(&shared.floor_spec(Path::new("recall.exe"), &read)), LIST, "before any hello: the whole list");
    shared.set_desired(Some(spec()), crate::record_dispatch::Sent::Waiting);
    shared.on_hello_floor(&json!({"type": "hello", "version": "0.1.0"})); // an old engine: no sync_hold
    assert_eq!(rx.try_recv().ok(), Some(json!({"type": ENGINE_OUTDATED})), "the notice is raised");
    let spawned = shared.floor_spec(Path::new("recall.exe"), &shared.spawn_sources.apply(Some(spec())).unwrap());
    assert_eq!(ids(&env_list(&spawned)), vec!["game", "out", "voice"], "no input, no picked device; game, default output and app stay");
    assert!(spawned.env.iter().any(|(k, v)| k == "RECALL_AUDIO_MIC_OFF" && v.as_deref() == Some("1")), "a very old engine opens no default mic");
    let (baked, current) = shared.spawn_sources.baked_and_current();
    assert!(crate::audio_intent::same_shape(&baked, &current), "no respawn loop");
    let manager = include_str!("supervisor_manager.rs").split_whitespace().collect::<Vec<_>>().join(" ");
    assert!(manager.contains("let spec = &shared.floor_spec(path, spec);"), "every spawn goes through it");
}
