use serde_json::{Value, json};

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
fn a_recovering_or_degraded_engine_still_has_a_saveable_replay_and_counts_as_recording() {
    let mut state = connected();
    // A recovering engine keeps its buffer and says why it is recovering; CPU copies record.
    for (encoder, error) in [("recovering", json!("NVENC encode failed: x")), ("degraded", Value::Null)] {
        apply(&mut state, json!({"mod":"engine","event":{"type":"status","buffer_secs":30.0,"encoder":encoder,"encoder_error":error}}));
        assert!(state.snapshot.replay_ready(), "{encoder}: the buffered clip is saveable");
    }
    apply(&mut state, json!({"mod":"engine","event":{"type":"status","buffer_secs":30.0,"encoder":"failed","encoder_error":"x"}}));
    assert!(!state.snapshot.replay_ready());
    for (session, busy) in [("degraded", true), ("recovering", true), ("recording", true), ("starting", true), ("error", false), ("waiting", false)] {
        let status = SessionStatus { state: session.into(), ..Default::default() };
        assert_eq!(status.capturing(), busy, "{session}");
    }
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
    apply(&mut state, json!({"mod":"session","event":{"type":"disk_low","drive":"C:","free_bytes":3,"need_bytes":5}}));
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
            DaemonEvent::RecordingStopped { path: "E:/rec.mp4".into(), seconds: 61.0, parts: 1, gap_seconds: 0.0, frozen_seconds: 0.0, locked_seconds: 0.0 },
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

/// Gate P2-d: `recording_recovered` (a `.part` renamed after a kill) was only logged.
#[test]
fn a_recovered_recording_is_an_event() {
    let mut state = connected();
    apply(&mut state, json!({"mod":"engine","event":{"type":"recording_recovered","path":"E:/c/G__rec.mp4"}}));
    apply(&mut state, json!({"mod":"engine","event":{"type":"recording_recovered","path":"E:/c/H__rec.mp4","gap_seconds":7.5,"frozen_seconds":40.0}}));
    // One event for several files (the engine lists them in `paths`; `path` is the first).
    apply(&mut state, json!({"mod":"engine","event":{"type":"recording_recovered","path":"E:/c/A__rec.mp4","paths":["E:/c/A__rec.mp4","E:/c/B__rec.mp4"]}}));
    assert_eq!(
        state.events,
        vec![
            DaemonEvent::RecordingRecovered { paths: vec!["E:/c/G__rec.mp4".into()], gap_seconds: 0.0, frozen_seconds: 0.0 },
            DaemonEvent::RecordingRecovered { paths: vec!["E:/c/H__rec.mp4".into()], gap_seconds: 7.5, frozen_seconds: 40.0 },
            DaemonEvent::RecordingRecovered { paths: vec!["E:/c/A__rec.mp4".into(), "E:/c/B__rec.mp4".into()], gap_seconds: 0.0, frozen_seconds: 0.0 },
        ]
    );
}

/// Merge gate 11-07 (P1): `continued` was ignored, so a part change read as
/// "Recording saved" then "Recording started".
#[test]
fn a_continued_recording_maps_to_its_own_events() {
    let mut state = connected();
    apply(&mut state, json!({"mod":"engine","event":{"type":"recording_started","path":"E:/G__rec.mp4","continued":false,"part":1,"cause":null}}));
    apply(&mut state, json!({"mod":"engine","event":{"type":"recording_stopped","path":"E:/G__rec.mp4","seconds":3540.0,"bytes":9,"continued":true,"parts":1,"total_seconds":3540.0,"cause":"size"}}));
    apply(&mut state, json!({"mod":"engine","event":{"type":"recording_started","path":"E:/G__rec-2.mp4","continued":true,"part":2,"cause":"size"}}));
    apply(&mut state, json!({"mod":"engine","event":{"type":"recording_stopped","path":"E:/G__rec-2.mp4","seconds":60.0,"bytes":9,"continued":false,"parts":2,"total_seconds":3600.0,"gap_seconds":7.5,"frozen_seconds":40.0,"cause":null}}));
    apply(&mut state, json!({"mod":"engine","event":{"type":"recording_failed","reason":"the video encoder failed","path":"E:/H__rec-2.mp4","paths":["E:/H__rec.mp4","E:/H__rec-2.mp4"]}}));
    apply(&mut state, json!({"mod":"engine","event":{"type":"recording_failed","reason":"old engine","path":"E:/I__rec.mp4"}}));
    assert_eq!(
        state.events,
        vec![
            DaemonEvent::RecordingStarted,
            DaemonEvent::RecordingPartSaved { path: "E:/G__rec.mp4".into() },
            DaemonEvent::RecordingContinued { part: 2, cause: "size".into() },
            DaemonEvent::RecordingStopped { path: "E:/G__rec-2.mp4".into(), seconds: 3600.0, parts: 2, gap_seconds: 7.5, frozen_seconds: 40.0, locked_seconds: 0.0 },
            DaemonEvent::RecordingFailed { reason: "the video encoder failed".into(), files: vec!["E:/H__rec.mp4".into(), "E:/H__rec-2.mp4".into()], gap_seconds: 0.0, frozen_seconds: 0.0 },
            DaemonEvent::RecordingFailed { reason: "old engine".into(), files: vec!["E:/I__rec.mp4".into()], gap_seconds: 0.0, frozen_seconds: 0.0 },
        ]
    );
}

#[test]
fn engine_one_shot_events_queue_in_order() {
    let mut state = connected();
    apply(&mut state, json!({"mod":"engine","event":{"type":"saved","path":"E:\\clip.mp4","seconds":119.5,"frames":7000,"bytes":123456,"elapsed_ms":850}}));
    apply(&mut state, json!({"mod":"engine","event":{"type":"saved","path":"E:/cut.mp4","seconds":3.0,"bytes":9,"elapsed_ms":1,"partial":"The save stopped making progress"}}));
    apply(&mut state, json!({"mod":"engine","event":{"type":"save_failed","reason":"buffer empty"}}));
    apply(&mut state, json!({"mod":"engine","event":{"type":"export_started","source":"a.mp4","path":"b.mp4","fps":60}}));
    apply(&mut state, json!({"mod":"engine","event":{"type":"exported","source":"a.mp4","path":"b.mp4","fps":60,"frames":600,"seconds":10.0,"elapsed_ms":900}}));
    apply(&mut state, json!({"mod":"engine","event":{"type":"export_failed","source":"a.mp4","reason":"nvenc"}}));
    assert!(!apply(&mut state, json!({"mod":"engine","event":{"type":"encoder_log","message":"opened"}})));
    assert_eq!(
        state.events,
        vec![
            DaemonEvent::Saved { path: "E:\\clip.mp4".into(), seconds: 119.5, bytes: 123456, elapsed_ms: 850, partial: None },
            DaemonEvent::Saved { path: "E:/cut.mp4".into(), seconds: 3.0, bytes: 9, elapsed_ms: 1, partial: Some("The save stopped making progress".into()) },
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
fn config_event_carries_overridden_keys_and_hotkey_warnings() {
    let mut state = connected();
    let line = json!({"mod":"session","event":{"type":"config","config":{"recording":{"output_dir":r"D:\Tmp"}},
        "overridden":["recording.output_dir"],"hotkey_warnings":["bookmark uses Alt+Z"]}});
    assert!(apply(&mut state, line));
    assert_eq!(state.snapshot.config_overridden, vec!["recording.output_dir".to_string()]);
    assert_eq!(state.snapshot.hotkey_warnings, vec!["bookmark uses Alt+Z".to_string()]);
    // An older recorder sends neither: both read empty.
    apply(&mut state, json!({"mod":"session","event":{"type":"config","config":{"a":1}}}));
    assert!(state.snapshot.config_overridden.is_empty() && state.snapshot.hotkey_warnings.is_empty());
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
        (DaemonCommand::QuitNow, json!({"cmd":"quit","force":true})),
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
        (DaemonCommand::RegisterGame { exe: "Game.exe".into(), how: "never" }, json!({"cmd":"register_game","exe":"Game.exe","how":"never"})),
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

#[test]
fn unknown_storage_warning_levels_never_toast() {
    // A newer recorder may add levels; only a known numeric level (1..=100) is a cap warning.
    let mut state = connected();
    for level in [json!("free"), json!("bogus"), json!(0), json!(150), Value::Null] {
        apply(&mut state, json!({"mod":"session","event":{"type":"storage_warning","level":level,"used_bytes":0,"cap_bytes":0}}));
    }
    apply(&mut state, json!({"mod":"session","event":{"type":"storage_warning","used_bytes":0,"cap_bytes":0}}));
    assert_eq!(state.events, Vec::<DaemonEvent>::new(), "no cap toast for a level this app does not know");
}

#[test]
fn a_message_type_the_app_does_not_know_is_ignored() {
    // How an app built before `disk_low` treats it: its parser has the same catch-all
    // `_ => {}` arm (protocol.rs before d57f46f, line 94), so no event and no toast.
    let mut state = connected();
    apply(&mut state, json!({"mod":"session","event":{"type":"disk_low_v2","drive":"C:","free_bytes":3,"need_bytes":5}}));
    assert_eq!(state.events, Vec::<DaemonEvent>::new());
}

/// Audit U8: the time locked during a recording and the unlock notice reach the app.
#[test]
fn a_lock_during_a_recording_reaches_the_app() {
    let mut state = connected();
    apply(&mut state, json!({"mod":"engine","event":{"type":"recording_stopped","path":"E:/G__rec.mp4","seconds":1200.0,"bytes":9,"continued":false,"parts":1,"total_seconds":1200.0,"gap_seconds":0.0,"frozen_seconds":0.0,"locked_seconds":720.0,"cause":null}}));
    apply(&mut state, json!({"mod":"session","event":{"type":"recording_locked","locked_seconds":720.0}}));
    assert_eq!(
        state.events,
        vec![
            DaemonEvent::RecordingStopped { path: "E:/G__rec.mp4".into(), seconds: 1200.0, parts: 1, gap_seconds: 0.0, frozen_seconds: 0.0, locked_seconds: 720.0 },
            DaemonEvent::RecordingKeptLocked { seconds: 720.0 },
        ]
    );
}

/// Gate 20b: the stopped-while-locked notice and a cancelled shutdown reach the app.
#[test]
fn a_stop_during_the_lock_and_a_cancelled_shutdown_reach_the_app() {
    let mut state = connected();
    apply(&mut state, json!({"mod":"session","event":{"type":"recording_stopped_locked","locked_seconds":300.0}}));
    apply(&mut state, json!({"mod":"session","event":{"type":"shutdown_cancelled","title":"Shutdown cancelled","detail":"Recording continues in a new file"}}));
    apply(&mut state, json!({"mod":"session","event":{"type":"recording_partly_locked","locked_seconds":720.0}}));
    assert_eq!(
        state.events,
        vec![DaemonEvent::RecordingStoppedLocked { seconds: 300.0 }, DaemonEvent::ShutdownCancelled, DaemonEvent::RecordingPartlyLocked { seconds: 720.0 }]
    );
}

#[test]
fn every_config_event_counts_and_a_loads_repairs_queue_one_notice() {
    let mut state = connected();
    let line = json!({"mod":"session","event":{"type":"config","config":{"a":1},
        "config_repaired":{"id":42,"count":2,"notes":["x","y"]}}});
    apply(&mut state, line.clone());
    assert!(apply(&mut state, line), "the same config again is still a fresh answer");
    assert_eq!(state.snapshot.config_seq, 2);
    let notices: Vec<_> = state.events.iter().filter(|e| matches!(e, DaemonEvent::ConfigRepaired { .. })).cloned().collect();
    assert_eq!(notices, vec![DaemonEvent::ConfigRepaired { count: 2 }], "one notice per load id");
    // A clean load (id 0) and an older recorder (no field) say nothing.
    apply(&mut state, json!({"mod":"session","event":{"type":"config","config":{"a":1},"config_repaired":{"id":0,"count":0}}}));
    apply(&mut state, json!({"mod":"session","event":{"type":"config","config":{"a":1}}}));
    assert_eq!(state.events.iter().filter(|e| matches!(e, DaemonEvent::ConfigRepaired { .. })).count(), 1);
}

#[test]
fn the_status_says_when_a_quit_is_finishing_a_clip() {
    // R7-overlay-1: the settings row shows "Finishing a clip…" from this.
    let mut state = State::default();
    apply(&mut state, json!({"mod":"session","event":{"type":"status","state":"recording","finishing_clip":true}}));
    assert!(state.snapshot.session.as_ref().unwrap().finishing_clip);
    apply(&mut state, json!({"mod":"session","event":{"type":"status","state":"recording","finishing_clip":false}}));
    assert!(!state.snapshot.session.as_ref().unwrap().finishing_clip);
}

#[test]
fn the_recorder_never_inherits_the_guided_setup_flag() {
    // Setup starts only its first window with RECALL_SETUP; the recorder that window starts
    // outlives it and opens every later window, so the flag must stop here.
    let exe = std::path::Path::new(r"C:\Recall\recall-hdr.exe");
    for owner in [None, Some(42)] {
        let command = super::controller::controller_command(exe, owner, 0);
        let removed = command.get_envs().any(|(key, value)| key.eq_ignore_ascii_case("RECALL_SETUP") && value.is_none());
        assert!(removed, "setup owner {owner:?}");
    }
}
