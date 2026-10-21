use serde_json::{Value, json};

use super::controller::{process_running, repo_controller, workspace_lists_controller};
use super::protocol::{State, command_line};
use super::*;

fn apply(state: &mut State, line: Value) -> bool {
    state.apply_line(&line.to_string())
}

fn connected() -> State {
    let mut state = State::default();
    assert!(state.set_connected(true));
    state
}

#[test]
fn record_only_session_status_is_lenient() {
    let mut state = connected();
    let line = json!({"mod":"session","event":{"type":"status","record_only":true,"cpu_mask":"0xffff0000","save_hotkey":"Alt+N","save_seconds":120,"mic_hotkey":"Alt+M"}});
    assert!(apply(&mut state, line));
    let session = state.snapshot.session.clone().expect("session parsed");
    assert_eq!(session.save_hotkey, "Alt+N");
    assert_eq!(session.mic_hotkey, "Alt+M");
    assert_eq!(session.save_seconds, 120);
    assert_eq!(session.state, "");
    assert_eq!(session.target, None);
    assert!(!session.mic_muted);
}

#[test]
fn new_session_status_parses_every_field() {
    let mut state = connected();
    let line = json!({"mod":"session","event":{"type":"status","state":"recording","target":{"name":"VALORANT","exe":"VALORANT-Win64-Shipping.exe","pid":4242},
        "engine_health":"running","auto_record":true,"save_hotkey":"Alt+N","mic_hotkey":"Alt+M","save_seconds":90,"mic_muted":true,
        "overlay":{"enabled":true,"corner":"top-right","running":false},"autostart":true,
        "hdr":{"available":true,"display":"PG27AQDP","hdr_on":true,"manual_pin":false,"auto":true}}});
    assert!(apply(&mut state, line));
    let session = state.snapshot.session.clone().unwrap();
    assert_eq!(session.state, "recording");
    assert_eq!(session.target, Some(Target { name: "VALORANT".into(), exe: "VALORANT-Win64-Shipping.exe".into(), pid: 4242 }));
    assert_eq!(session.engine_health, "running");
    assert!(session.auto_record && session.mic_muted && session.autostart);
    assert_eq!(session.save_seconds, 90);
    assert_eq!(session.overlay, OverlayStatus { enabled: true, corner: "top-right".into(), running: false });
    assert_eq!(session.hdr, HdrStatus { available: true, display: Some("PG27AQDP".into()), hdr_on: true, manual_pin: false, auto: true });
}

#[test]
fn bad_field_shapes_are_dropped_not_fatal() {
    let mut state = connected();
    let line = json!({"mod":"session","event":{"type":"status","state":"waiting","target":"VALORANT","save_seconds":"lots","engine_health":null}});
    assert!(apply(&mut state, line));
    let session = state.snapshot.session.clone().unwrap();
    assert_eq!(session.state, "waiting");
    assert_eq!(session.target, None);
    assert_eq!(session.save_seconds, 0);
}

#[test]
fn session_fields_absent_from_a_later_status_are_kept() {
    let mut state = connected();
    apply(&mut state, json!({"mod":"session","event":{"type":"status","state":"recording","target":{"name":"VALORANT","exe":"v.exe","pid":1}}}));
    apply(&mut state, json!({"mod":"session","event":{"type":"status","save_seconds":120}}));
    let session = state.snapshot.session.clone().unwrap();
    assert_eq!(session.state, "recording");
    assert_eq!(session.save_seconds, 120);
    apply(&mut state, json!({"mod":"session","event":{"type":"status","target":null}}));
    assert_eq!(state.snapshot.session.as_ref().unwrap().target, None);
}

#[test]
fn engine_status_parses_and_drives_replay_ready() {
    let mut state = connected();
    let line = json!({"mod":"engine","event":{"type":"status","display":"hdr","max_nits":1000.0,"mode":"tonemap","select":"auto","source":"VALORANT",
        "width":2560,"height":1440,"fps":60,"buffer_secs":42.5,"encoder":"h264_nvenc","encoder_error":null,"recording_mode":"vfr","mic_muted":true}});
    assert!(apply(&mut state, line.clone()));
    let engine = state.snapshot.engine.clone().unwrap();
    assert_eq!((engine.width, engine.height, engine.fps), (2560, 1440, 60));
    assert_eq!(engine.buffer_secs, 42.5);
    assert_eq!(engine.encoder, "h264_nvenc");
    assert_eq!((engine.display.as_str(), engine.mode.as_str(), engine.select.as_str()), ("hdr", "tonemap", "auto"));
    assert!(state.snapshot.replay_ready());
    assert!(state.snapshot.session.as_ref().unwrap().mic_muted, "engine mic state mirrors into the session");
    assert!(!apply(&mut state, line), "an identical status is not a change");
    apply(&mut state, json!({"mod":"engine","event":{"type":"hello","version":"0.1.0","monitors":[]}}));
    assert!(!state.snapshot.replay_ready(), "a restarted engine has an empty buffer");
}

#[test]
fn legacy_hdr_status_maps_into_session() {
    let mut state = connected();
    let line = json!({"mod":"hdr","event":{"type":"status","target":"PG27AQDP","hdr":true,"manual_pin":true,"session_pid":null,"auto":false,"engine_health":"restarting"}});
    assert!(apply(&mut state, line));
    let session = state.snapshot.session.clone().unwrap();
    assert_eq!(session.hdr, HdrStatus { available: true, display: Some("PG27AQDP".into()), hdr_on: true, manual_pin: true, auto: false });
    assert_eq!(session.engine_health, "restarting");
    apply(&mut state, json!({"mod":"session","event":{"type":"status","record_only":true,"save_hotkey":"Alt+N"}}));
    let session = state.snapshot.session.clone().unwrap();
    assert!(session.hdr.hdr_on, "hdr fields survive a session status without them");
    assert_eq!(session.save_hotkey, "Alt+N");
}

#[test]
fn hotkey_and_recording_events_and_session_fields() {
    let mut state = connected();
    apply(&mut state, json!({"mod":"engine","event":{"type":"bookmarked","at_secs":12.5}}));
    apply(&mut state, json!({"mod":"engine","event":{"type":"screenshot","path":"V:/shots/a.png"}}));
    apply(&mut state, json!({"mod":"engine","event":{"type":"recording_started","path":"V:/rec.mp4"}}));
    apply(&mut state, json!({"mod":"engine","event":{"type":"recording_stopped","path":"V:/rec.mp4","seconds":61.0,"bytes":9}}));
    apply(&mut state, json!({"mod":"session","event":{"type":"storage_cleaned","removed":3,"freed_bytes":1000}}));
    assert_eq!(
        state.events,
        vec![
            DaemonEvent::Bookmarked,
            DaemonEvent::Screenshot { path: "V:/shots/a.png".into() },
            DaemonEvent::RecordingStarted,
            DaemonEvent::RecordingStopped { path: "V:/rec.mp4".into(), seconds: 61.0 },
            DaemonEvent::StorageCleaned { removed: 3, freed_bytes: 1000 },
        ]
    );
    apply(&mut state, json!({"mod":"session","event":{"type":"status","state":"recording","paused":false,"advanced_enabled":true,
        "leader_label":"Alt `","chord_save_seconds":30,"long_recording":{"active":true,"path":"V:/rec.mp4","started_secs":1792621900.0}}}));
    let session = state.snapshot.session.clone().unwrap();
    assert!(session.advanced_enabled && session.long_recording.active);
    assert_eq!(session.leader_label, "Alt `");
    assert_eq!(session.chord_save_seconds, 30);
}

#[test]
fn engine_one_shot_events_queue_in_order() {
    let mut state = connected();
    apply(&mut state, json!({"mod":"engine","event":{"type":"saved","path":"V:\\clip.mp4","seconds":119.5,"frames":7000,"bytes":123456,"elapsed_ms":850}}));
    apply(&mut state, json!({"mod":"engine","event":{"type":"save_failed","reason":"buffer empty"}}));
    apply(&mut state, json!({"mod":"engine","event":{"type":"export_started","source":"a.mp4","path":"b.mp4","fps":60}}));
    apply(&mut state, json!({"mod":"engine","event":{"type":"exported","source":"a.mp4","path":"b.mp4","fps":60,"frames":600,"seconds":10.0,"elapsed_ms":900}}));
    apply(&mut state, json!({"mod":"engine","event":{"type":"export_failed","source":"a.mp4","reason":"nvenc"}}));
    assert!(!apply(&mut state, json!({"mod":"engine","event":{"type":"encoder_log","message":"opened"}})));
    assert_eq!(
        state.events,
        vec![
            DaemonEvent::Saved { path: "V:\\clip.mp4".into(), seconds: 119.5, bytes: 123456, elapsed_ms: 850 },
            DaemonEvent::SaveFailed { reason: "buffer empty".into() },
            DaemonEvent::ExportStarted { source: "a.mp4".into(), path: "b.mp4".into(), fps: 60 },
            DaemonEvent::Exported { source: "a.mp4".into(), path: "b.mp4".into(), fps: 60, frames: 600, seconds: 10.0 },
            DaemonEvent::ExportFailed { source: "a.mp4".into(), reason: "nvenc".into() },
        ]
    );
}

#[test]
fn mic_muted_updates_session_and_emits() {
    let mut state = connected();
    apply(&mut state, json!({"mod":"session","event":{"type":"status","state":"recording"}}));
    assert!(apply(&mut state, json!({"mod":"engine","event":{"type":"mic_muted","muted":true}})));
    assert!(state.snapshot.session.as_ref().unwrap().mic_muted);
    assert_eq!(state.events, vec![DaemonEvent::MicMuted(true)]);
}

#[test]
fn config_ac_profile_and_error_events() {
    let mut state = connected();
    let config = json!({"overlay":{"enabled":true,"corner":"top-left"},"replay":{"seconds":120}});
    assert!(apply(&mut state, json!({"mod":"session","event":{"type":"config","config":config.clone()}})));
    assert_eq!(state.snapshot.config, Some(config));
    apply(&mut state, json!({"mod":"hdr","event":{"type":"ac_profile","profile":"PG27AQDP_Punchy-Gamma2.2_MHC2.icm","ok":true}}));
    apply(&mut state, json!({"mod":"hdr","event":{"type":"ac_profile","ok":false,"error":"no display"}}));
    apply(&mut state, json!({"mod":"session","event":{"type":"error","message":"set_config: unknown key"}}));
    assert_eq!(
        state.events,
        vec![
            DaemonEvent::AcProfile { ok: true, profile: Some("PG27AQDP_Punchy-Gamma2.2_MHC2.icm".into()), error: None },
            DaemonEvent::AcProfile { ok: false, profile: None, error: Some("no display".into()) },
            DaemonEvent::Error("set_config: unknown key".into()),
        ]
    );
}

#[test]
fn disconnect_clears_live_status_and_garbage_is_ignored() {
    let mut state = connected();
    apply(&mut state, json!({"mod":"engine","event":{"type":"status","encoder":"h264_nvenc","buffer_secs":5.0}}));
    apply(&mut state, json!({"mod":"session","event":{"type":"config","config":{"a":1}}}));
    assert!(!state.apply_line("not json"));
    assert!(!state.apply_line(r#"{"mod":"hub","event":{"type":"hello","daemon":"recall-hdr","version":"0.1.0"}}"#));
    assert!(state.set_connected(false));
    assert_eq!(state.snapshot.engine, None);
    assert_eq!(state.snapshot.session, None);
    assert!(state.snapshot.config.is_some(), "last config stays for the settings view");
    assert!(!state.set_connected(false));
}

#[test]
fn commands_serialize_to_controller_json() {
    let cases: Vec<(DaemonCommand, Value)> = vec![
        (DaemonCommand::Status, json!({"cmd":"status"})),
        (DaemonCommand::Quit, json!({"cmd":"quit"})),
        (DaemonCommand::SaveClip { seconds: None }, json!({"cmd":"save_clip"})),
        (DaemonCommand::SaveClip { seconds: Some(30) }, json!({"cmd":"save_clip","seconds":30})),
        (DaemonCommand::SetMicMute(true), json!({"cmd":"set_mic_mute","muted":true})),
        (DaemonCommand::ToggleMicMute, json!({"cmd":"toggle_mic_mute"})),
        (DaemonCommand::Bookmark, json!({"cmd":"bookmark"})),
        (DaemonCommand::Screenshot, json!({"cmd":"screenshot"})),
        (DaemonCommand::RecordToggle, json!({"cmd":"record_toggle"})),
        (DaemonCommand::Pause, json!({"cmd":"pause"})),
        (DaemonCommand::Resume, json!({"cmd":"resume"})),
        (DaemonCommand::ToggleHdr, json!({"cmd":"toggle_hdr"})),
        (DaemonCommand::SetMode("hdr".into()), json!({"cmd":"set_mode","value":"hdr"})),
        (DaemonCommand::SetAcProfile("punchy".into()), json!({"cmd":"set_ac_profile","variant":"punchy"})),
        (DaemonCommand::GetConfig, json!({"cmd":"get_config"})),
        (DaemonCommand::SetConfig(json!({"overlay":{"enabled":true}})), json!({"cmd":"set_config","patch":{"overlay":{"enabled":true}}})),
    ];
    for (command, expected) in cases {
        let line = command_line(&command);
        assert!(!line.contains('\n'), "one line per command");
        assert_eq!(serde_json::from_str::<Value>(&line).unwrap(), expected, "{command:?}");
    }
    assert_eq!(command_line(&DaemonCommand::SaveClip { seconds: Some(120) }), r#"{"cmd":"save_clip","seconds":120}"#);
}

#[test]
fn send_without_connection_is_dropped() {
    let shared = Shared::new();
    let client = DaemonClient { shared: Arc::new(shared) };
    assert!(!client.send(DaemonCommand::Status));
    assert_eq!(client.revision(), 0);
    assert!(!client.snapshot().connected);
}

fn wait_until(what: &str, condition: impl Fn() -> bool) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while !condition() {
        assert!(std::time::Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
}

/// Real pipe, real threads: a fake controller on a private pipe name sees the two
/// connect requests and a UI command; its lines reach the snapshot; closing it marks
/// the client disconnected and sends are dropped.
#[test]
fn pipe_client_round_trip_against_a_fake_controller() {
    use interprocess::local_socket::traits::{Listener as _, Stream as _};
    use interprocess::local_socket::{GenericNamespaced, ListenerOptions, ToNsName};
    use std::io::{BufRead as _, BufReader, Write as _};

    let pipe = format!("recall-native-test-{}", std::process::id());
    let listener = ListenerOptions::new().name(pipe.as_str().to_ns_name::<GenericNamespaced>().unwrap()).create_sync().unwrap();
    let client = DaemonClient::spawn_on(&pipe);
    let (accepted_tx, accepted_rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let stream = listener.accept().unwrap();
        let _ = accepted_tx.send(stream);
        std::thread::sleep(std::time::Duration::from_secs(5)); // keep the listener alive briefly
    });
    let stream = accepted_rx.recv_timeout(std::time::Duration::from_secs(10)).expect("client connected");
    let (reader, mut writer) = stream.split();
    let mut lines = BufReader::new(reader).lines();
    assert_eq!(lines.next().unwrap().unwrap(), r#"{"cmd":"status"}"#);
    assert_eq!(lines.next().unwrap().unwrap(), r#"{"cmd":"get_config"}"#);
    wait_until("connected", || client.snapshot().connected);
    writer.write_all(b"{\"mod\":\"session\",\"event\":{\"type\":\"status\",\"state\":\"recording\",\"save_seconds\":120}}\n").unwrap();
    writer.write_all(b"{\"mod\":\"engine\",\"event\":{\"type\":\"saved\",\"path\":\"V:\\\\c.mp4\",\"seconds\":12.5,\"bytes\":9,\"elapsed_ms\":3}}\n").unwrap();
    wait_until("session status", || client.snapshot().session.is_some_and(|s| s.state == "recording"));
    wait_until("saved event", || client.revision() >= 3);
    assert_eq!(client.drain_events(), vec![DaemonEvent::Saved { path: "V:\\c.mp4".into(), seconds: 12.5, bytes: 9, elapsed_ms: 3 }]);
    assert!(client.drain_events().is_empty(), "drained once");
    assert!(client.send(DaemonCommand::SaveClip { seconds: Some(30) }));
    assert_eq!(lines.next().unwrap().unwrap(), r#"{"cmd":"save_clip","seconds":30}"#);
    drop(writer);
    drop(lines);
    wait_until("disconnect", || !client.snapshot().connected);
    assert_eq!(client.snapshot().session, None, "stale status cleared");
}

#[test]
fn workspace_manifest_detection() {
    assert!(workspace_lists_controller("[workspace]\nresolver = \"2\"\nmembers = [\"recall\", \"recall-hdr\", \"recall-overlay\"]\n"));
    assert!(workspace_lists_controller("[workspace]\nmembers = [\n  \"recall\",\n  \"recall-hdr\",\n]\n"));
    assert!(!workspace_lists_controller("[workspace]\n"));
    assert!(!workspace_lists_controller("[package]\nname = \"recall-hdr\"\n"));
    assert!(!workspace_lists_controller("[workspace]\nmembers = [\"recall-hdr-extra\"]\n"));
}

#[test]
fn repo_controller_walks_up_to_the_workspace() {
    let root = std::env::temp_dir().join(format!("recall-native-controller-test-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let exe_dir = root.join("target-gpui").join("debug");
    std::fs::create_dir_all(&exe_dir).unwrap();
    std::fs::write(root.join("Cargo.toml"), "[workspace]\nmembers = [\"recall\", \"recall-hdr\"]\n").unwrap();
    let exe = exe_dir.join("recall-native.exe");
    assert_eq!(repo_controller(&exe), None, "no build yet");
    let debug = root.join("target-dev").join("debug");
    std::fs::create_dir_all(&debug).unwrap();
    std::fs::write(debug.join("recall-hdr.exe"), b"stub").unwrap();
    assert_eq!(repo_controller(&exe), Some(debug.join("recall-hdr.exe")));
    let release = root.join("target-dev").join("release");
    std::fs::create_dir_all(&release).unwrap();
    std::fs::write(release.join("recall-hdr.exe"), b"stub").unwrap();
    assert_eq!(repo_controller(&exe), Some(release.join("recall-hdr.exe")), "release preferred");
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn process_lookup_finds_this_test_binary_only() {
    let me = std::env::current_exe().unwrap();
    let name = me.file_name().unwrap().to_string_lossy().to_uppercase();
    assert!(process_running(&name).unwrap());
    assert!(!process_running("recall-native-no-such-process-7f3a.exe").unwrap());
}
