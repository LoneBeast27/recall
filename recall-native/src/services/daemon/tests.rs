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
        "hdr":{"available":true,"display":"Monitor 27Q","hdr_on":true,"manual_pin":false,"auto":true}}});
    assert!(apply(&mut state, line));
    let session = state.snapshot.session.clone().unwrap();
    assert_eq!(session.state, "recording");
    assert_eq!(session.target, Some(Target { name: "VALORANT".into(), exe: "VALORANT-Win64-Shipping.exe".into(), pid: 4242 }));
    assert_eq!(session.engine_health, "running");
    assert!(session.auto_record && session.mic_muted && session.autostart);
    assert_eq!(session.save_seconds, 90);
    assert_eq!(session.overlay, OverlayStatus { enabled: true, corner: "top-right".into(), running: false });
    assert_eq!(session.hdr, HdrStatus { available: true, display: Some("Monitor 27Q".into()), hdr_on: true, manual_pin: false, auto: true });
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
fn stats_island_status_fields_parse_and_default_on_older_controllers() {
    let mut state = connected();
    apply(&mut state, json!({"mod":"session","event":{"type":"status","state":"waiting","save_seconds":120}}));
    let old = state.snapshot.session.clone().unwrap();
    assert!(!old.overlay_stats, "older controllers: stats off");
    assert_eq!(old.fps_source, "", "older controllers: no FPS source");
    apply(&mut state, json!({"mod":"session","event":{"type":"status","overlay_stats":true,"fps_source":"no_permission",
        "overlay":{"enabled":false,"corner":"top-right","running":true,"stats_style":"minimal"}}}));
    let session = state.snapshot.session.clone().unwrap();
    assert!(session.overlay_stats);
    assert_eq!(session.fps_source, "no_permission");
    assert!(session.overlay.running && !session.overlay.enabled, "unknown stats_style is ignored");
    assert_eq!(session.save_seconds, 120, "earlier fields kept");
    apply(&mut state, json!({"mod":"session","event":{"type":"status","fps_source":"etw"}}));
    assert_eq!(state.snapshot.session.as_ref().unwrap().fps_source, "etw");
    assert!(state.snapshot.session.as_ref().unwrap().overlay_stats);
    apply(&mut state, json!({"mod":"session","event":{"type":"status","overlay_stats":"yes"}}));
    assert!(!state.snapshot.session.as_ref().unwrap().overlay_stats, "a wrong shape drops to the default");
}

#[test]
fn island_notices_are_not_queued_as_app_events() {
    let mut state = connected();
    let notice = json!({"mod":"session","event":{"type":"notice","kind":"game","title":"Recording","detail":"VALORANT"}});
    assert!(!apply(&mut state, notice), "the overlay shows notices; the app's pill already shows the state");
    assert!(state.events.is_empty());
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
    let line = json!({"mod":"hdr","event":{"type":"status","target":"Monitor 27Q","hdr":true,"manual_pin":true,"session_pid":null,"auto":false,"engine_health":"restarting"}});
    assert!(apply(&mut state, line));
    let session = state.snapshot.session.clone().unwrap();
    assert_eq!(session.hdr, HdrStatus { available: true, display: Some("Monitor 27Q".into()), hdr_on: true, manual_pin: true, auto: false });
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
    apply(&mut state, json!({"mod":"engine","event":{"type":"screenshot","path":"E:/shots/a.png"}}));
    apply(&mut state, json!({"mod":"engine","event":{"type":"recording_started","path":"E:/rec.mp4"}}));
    apply(&mut state, json!({"mod":"engine","event":{"type":"recording_stopped","path":"E:/rec.mp4","seconds":61.0,"bytes":9}}));
    apply(&mut state, json!({"mod":"session","event":{"type":"storage_cleaned","removed":3,"freed_bytes":1000}}));
    apply(&mut state, json!({"mod":"session","event":{"type":"storage_cleaned","removed":2,"freed_bytes":9,"through":1_790_000_000u64,"used_bytes":80,"cap_bytes":100}}));
    apply(&mut state, json!({"mod":"session","event":{"type":"storage_warning","level":90,"used_bytes":91,"cap_bytes":100,"next_through":1_780_000_000u64}}));
    apply(&mut state, json!({"mod":"session","event":{"type":"storage_warning","level":100,"used_bytes":120,"cap_bytes":100,"next_through":null}}));
    apply(&mut state, json!({"mod":"session","event":{"type":"storage_warning","level":"free","drive":"C:","free_bytes":3,"need_bytes":5}}));
    apply(&mut state, json!({"mod":"session","event":{"type":"storage_preview","cap_gb":1000,"used_bytes":5,"removed":4,"freed_bytes":3,"through":1_770_000_000u64}}));
    apply(&mut state, json!({"mod":"engine","event":{"type":"clip_moved","path":"E:/a.mp4","elapsed_ms":900}}));
    apply(&mut state, json!({"mod":"engine","event":{"type":"clip_moved","path":"E:/b.mp4","elapsed_ms":900,"kept_path":"F:/s/kept/b.mp4"}}));
    apply(&mut state, json!({"mod":"engine","event":{"type":"clip_move_failed","path":"E:/a.mp4","reason":"disk full"}}));
    assert_eq!(
        state.events,
        vec![
            DaemonEvent::Bookmarked,
            DaemonEvent::Screenshot { path: "E:/shots/a.png".into() },
            DaemonEvent::RecordingStarted,
            DaemonEvent::RecordingStopped { path: "E:/rec.mp4".into(), seconds: 61.0 },
            DaemonEvent::StorageCleaned { removed: 3, freed_bytes: 1000, through: None, used_bytes: 0, cap_bytes: 0 },
            DaemonEvent::StorageCleaned { removed: 2, freed_bytes: 9, through: Some(1_790_000_000), used_bytes: 80, cap_bytes: 100 },
            DaemonEvent::StorageWarning { level: 90, used_bytes: 91, cap_bytes: 100, next_through: Some(1_780_000_000) },
            DaemonEvent::StorageWarning { level: 100, used_bytes: 120, cap_bytes: 100, next_through: None },
            DaemonEvent::DiskLow { drive: "C:".into(), free_bytes: 3, need_bytes: 5 },
            DaemonEvent::StoragePreview { cap_gb: 1000, used_bytes: 5, removed: 4, freed_bytes: 3, through: Some(1_770_000_000) },
            DaemonEvent::ClipMoved { path: "E:/a.mp4".into(), kept_path: None },
            DaemonEvent::ClipMoved { path: "E:/b.mp4".into(), kept_path: Some("F:/s/kept/b.mp4".into()) },
            DaemonEvent::ClipMoveFailed { path: "E:/a.mp4".into(), reason: "disk full".into() },
        ]
    );
    apply(&mut state, json!({"mod":"session","event":{"type":"status","state":"recording","paused":false,
        "stats_hotkey":"Alt+P","overlay_hotkey":"Alt+O","editor_hotkey":"Alt+U","long_recording":{"active":true,"path":"E:/rec.mp4","started_secs":1792621900.0}}}));
    let session = state.snapshot.session.clone().unwrap();
    assert!(session.long_recording.active);
    assert_eq!(session.stats_hotkey, "Alt+P");
    assert_eq!(session.overlay_hotkey, "Alt+O");
    assert_eq!(session.editor_hotkey, "Alt+U");
}

#[test]
fn engine_one_shot_events_queue_in_order() {
    let mut state = connected();
    apply(&mut state, json!({"mod":"engine","event":{"type":"saved","path":"E:\\clip.mp4","seconds":119.5,"frames":7000,"bytes":123456,"elapsed_ms":850}}));
    apply(&mut state, json!({"mod":"engine","event":{"type":"save_failed","reason":"buffer empty"}}));
    apply(&mut state, json!({"mod":"engine","event":{"type":"export_started","source":"a.mp4","path":"b.mp4","fps":60}}));
    apply(&mut state, json!({"mod":"engine","event":{"type":"exported","source":"a.mp4","path":"b.mp4","fps":60,"frames":600,"seconds":10.0,"elapsed_ms":900}}));
    apply(&mut state, json!({"mod":"engine","event":{"type":"export_failed","source":"a.mp4","reason":"nvenc"}}));
    assert!(!apply(&mut state, json!({"mod":"engine","event":{"type":"encoder_log","message":"opened"}})));
    assert_eq!(
        state.events,
        vec![
            DaemonEvent::Saved { path: "E:\\clip.mp4".into(), seconds: 119.5, bytes: 123456, elapsed_ms: 850 },
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
    apply(&mut state, json!({"mod":"hdr","event":{"type":"ac_profile","profile":"Punchy-Gamma2.2_MHC2.icm","ok":true}}));
    apply(&mut state, json!({"mod":"hdr","event":{"type":"ac_profile","ok":false,"error":"no display"}}));
    apply(&mut state, json!({"mod":"session","event":{"type":"error","message":"set_config: unknown key"}}));
    assert_eq!(
        state.events,
        vec![
            DaemonEvent::AcProfile { ok: true, profile: Some("Punchy-Gamma2.2_MHC2.icm".into()), error: None },
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
        (DaemonCommand::ToggleStats, json!({"cmd":"toggle_stats"})),
        (DaemonCommand::SetConfig(json!({"overlay":{"stats":true}})), json!({"cmd":"set_config","patch":{"overlay":{"stats":true}}})),
        (DaemonCommand::SetMode("hdr".into()), json!({"cmd":"set_mode","value":"hdr"})),
        (DaemonCommand::SetAcProfile("punchy".into()), json!({"cmd":"set_ac_profile","variant":"punchy"})),
        (DaemonCommand::GetConfig, json!({"cmd":"get_config"})),
        (DaemonCommand::StoragePreview(500), json!({"cmd":"storage_preview","cap_gb":500})),
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
    writer.write_all(b"{\"mod\":\"engine\",\"event\":{\"type\":\"saved\",\"path\":\"E:\\\\c.mp4\",\"seconds\":12.5,\"bytes\":9,\"elapsed_ms\":3}}\n").unwrap();
    wait_until("session status", || client.snapshot().session.is_some_and(|s| s.state == "recording"));
    wait_until("saved event", || client.revision() >= 3);
    assert_eq!(client.drain_events(), vec![DaemonEvent::Saved { path: "E:\\c.mp4".into(), seconds: 12.5, bytes: 9, elapsed_ms: 3 }]);
    assert!(client.drain_events().is_empty(), "drained once");
    assert!(client.send(DaemonCommand::SaveClip { seconds: Some(30) }));
    // A status ping (every 2 s) may come first on a slow run.
    let sent = lines.by_ref().map(Result::unwrap).find(|l| l != r#"{"cmd":"status"}"#).unwrap();
    assert_eq!(sent, r#"{"cmd":"save_clip","seconds":30}"#);
    // Idle, the client asks for status by itself: only a live pump answers.
    let t = std::time::Instant::now();
    assert_eq!(lines.next().unwrap().unwrap(), r#"{"cmd":"status"}"#);
    assert!(t.elapsed() < std::time::Duration::from_secs(4), "ping after {:?}", t.elapsed());
    drop(writer);
    drop(lines);
    wait_until("disconnect", || !client.snapshot().connected);
    assert_eq!(client.snapshot().session, None, "stale status cleared");
}

/// The recorder quits (an install) and a new one starts on the same pipe: the app keeps
/// running, sees the disconnect, then reconnects by itself and asks for status again.
#[test]
fn pipe_client_reconnects_to_a_restarted_controller() {
    use interprocess::local_socket::traits::{Listener as _, Stream as _};
    use interprocess::local_socket::{GenericNamespaced, ListenerOptions, ToNsName};
    use std::io::{BufRead as _, BufReader, Write as _};

    let pipe = format!("recall-native-restart-test-{}", std::process::id());
    let listen = || ListenerOptions::new().name(pipe.as_str().to_ns_name::<GenericNamespaced>().unwrap()).create_sync().unwrap();
    let client = DaemonClient::spawn_on(&pipe);
    for run in 1..=2 {
        let stream = listen().accept().unwrap(); // the listener goes with this scope, like a quit recorder
        let (reader, mut writer) = stream.split();
        let mut lines = BufReader::new(reader).lines();
        assert_eq!(lines.next().unwrap().unwrap(), r#"{"cmd":"status"}"#, "run {run} asks for status");
        assert_eq!(lines.next().unwrap().unwrap(), r#"{"cmd":"get_config"}"#);
        wait_until("connected", || client.snapshot().connected);
        writer.write_all(b"{\"mod\":\"session\",\"event\":{\"type\":\"status\",\"state\":\"recording\",\"save_seconds\":120}}\n").unwrap();
        wait_until("session status", || client.snapshot().session.is_some_and(|s| s.state == "recording"));
        assert!(client.send(DaemonCommand::SaveClip { seconds: None }), "run {run} takes commands");
        drop(writer);
        drop(lines);
        wait_until("disconnect", || !client.snapshot().connected);
        assert!(!client.send(DaemonCommand::Status), "no recorder: commands are dropped, the app stays up");
    }
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

/// 11-05: the engine stopped 30 s after sobosuba closed, the controller said
/// "waiting" + engine "stopped", and the pill still read "sobosuba 2:00" from the
/// engine's last status (it sends no goodbye).
#[test]
fn a_stopped_engine_drops_its_last_status() {
    let mut state = connected();
    apply(&mut state, json!({"mod":"engine","event":{"type":"status","source":"sobosuba","fps":60,"buffer_secs":120.0,"encoder":"h264_nvenc"}}));
    apply(&mut state, json!({"mod":"session","event":{"type":"status","state":"waiting","engine_health":"running","target":null}}));
    assert!(state.snapshot.replay_ready(), "a lingering engine still holds the clip");
    assert!(apply(&mut state, json!({"mod":"session","event":{"type":"status","state":"waiting","engine_health":"stopped","target":null}})));
    assert_eq!(state.snapshot.engine, None);
    assert!(!state.snapshot.replay_ready());
    for health in ["off", "restarting"] {
        apply(&mut state, json!({"mod":"engine","event":{"type":"status","source":"A","buffer_secs":5.0,"encoder":"h264_nvenc"}}));
        apply(&mut state, json!({"mod":"session","event":{"type":"status","engine_health":health}}));
        assert_eq!(state.snapshot.engine, None, "{health}");
    }
}

#[test]
fn a_drain_parses_and_counts_down_from_its_end_time() {
    let mut state = connected();
    apply(&mut state, json!({"mod":"session","event":{"type":"status","state":"draining","engine_health":"running","target":null,
        "drain":{"name":"sobosuba","total_secs":120,"ends_unix_ms":1_000_000}}}));
    let session = state.snapshot.session.clone().unwrap();
    let drain = session.drain.clone().unwrap();
    assert_eq!((session.state.as_str(), drain.name.as_str(), drain.total_secs), ("draining", "sobosuba", 120));
    assert_eq!(drain.remaining_secs(1_000_000 - 120_000), 120);
    assert_eq!(drain.remaining_secs(1_000_000 - 119_500), 120, "rounded up");
    assert_eq!(drain.remaining_secs(1_000_000 - 1), 1);
    assert_eq!(drain.remaining_secs(1_000_000), 0);
    assert_eq!(drain.remaining_secs(2_000_000), 0);
    assert_eq!(drain.remaining_secs(0), 120, "a clock behind the controller never shows more than the buffer");
    apply(&mut state, json!({"mod":"session","event":{"type":"status","state":"waiting","engine_health":"stopped","drain":null}}));
    assert_eq!(state.snapshot.session.unwrap().drain, None);
}

#[test]
fn no_status_reply_for_five_seconds_is_unresponsive() {
    use std::time::{Duration, Instant};
    let limit = Duration::from_secs(5);
    let mut state = State::default();
    assert!(!state.unresponsive(Instant::now() + limit * 2, limit), "disconnected is not unresponsive");
    state.set_connected(true);
    let t0 = Instant::now();
    assert!(!state.unresponsive(t0, limit), "a fresh connection has its grace");
    assert!(state.unresponsive(t0 + Duration::from_secs(6), limit));
    apply(&mut state, json!({"mod":"engine","event":{"type":"status","buffer_secs":1.0}}));
    assert!(state.unresponsive(Instant::now() + Duration::from_secs(6), limit), "engine lines do not prove the pump is alive");
    apply(&mut state, json!({"mod":"session","event":{"type":"status","state":"waiting"}}));
    assert!(!state.unresponsive(Instant::now() + Duration::from_secs(4), limit));
}
