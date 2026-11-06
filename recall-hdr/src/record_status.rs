//! The record mode's `mod:"session"` status event and tray line, built from
//! plain inputs so the exact wire shape is unit-tested.
//!
//! ```json
//! {"type":"status","mode":"record","state":"waiting|starting|recording|degraded|recovering|draining|paused|error",
//!  "paused":B,"target":{"name":S,"exe":S|null,"pid":N|null}|null,
//!  "engine_health":"running|restarting|off|stopped","auto_record":B,
//!  "save_hotkey":S,"mic_hotkey":S,"save_seconds":N,
//!  "save_hotkeys":[{"hotkey":S,"seconds":N}],   // seconds 0 = full buffer
//!  "bookmark_hotkey":S,"screenshot_hotkey":S,"record_hotkey":S,"stats_hotkey":S,"overlay_hotkey":S,"editor_hotkey":S,  // "" = off
//!  "hotkey_problems":[S],   // hotkeys that did not register (taken by another app)
//!  "mic_muted":B,
//!  "long_recording":{"active":B,"path":S|null,"started_secs":F|null},  // Unix seconds
//!  "pending_restart":B,   // a settings restart / retarget waits for a save / long recording to end
//!  "overlay":{"enabled":B,"corner":S,"running":B,"stats_style":S},"autostart":B,
//!  "overlay_stats":B,     // the performance-stats island is on
//!  "fps_source":"etw|no_permission|off",   // `frame_timing` (off while stats are off)
//!  "storage":{"cap_gb":N},
//!  "drain":{"name":S,"total_secs":N,"ends_unix_ms":N}|null,  // `record_drain`: the closed game's last clip
//!  "hdr":{"available":B,"display":S|null,"hdr_on":B,"manual_pin":B,"auto":B}}
//! ```

use serde_json::{json, Value};

use crate::app::health_summary;
use crate::config::Config;
use crate::record_hdr::HdrSnapshot;
use crate::record_engine::EngineTarget;
use crate::record_target::DESKTOP_NAME;
use crate::supervisor::EngineHealth;

pub struct StatusInputs<'a> {
    pub wanted: &'a EngineTarget,
    pub health: EngineHealth,
    /// Last engine `status.encoder`.
    pub encoder: Option<&'a str>,
    /// The engine has said `hello` since the last retarget (its status is ours).
    pub fresh_engine: bool,
    pub cfg: &'a Config,
    pub mic_muted: bool,
    pub overlay_running: bool,
    /// `frame_timing::FpsSource::as_str` ("off" while stats are off).
    pub fps_source: &'a str,
    pub hdr: &'a HdrSnapshot,
    pub paused: bool,
    /// `EngineActivity::long_recording_json`.
    pub long_recording: Value,
    pub pending_restart: bool,
    /// `record_drain::DrainClock::status`: the closed game's last clip is still saveable.
    pub drain: Option<Value>,
    /// Hotkeys that did not register (`RecordHotkeys::bind` warnings).
    pub hotkey_problems: &'a [String],
}

/// `paused` (the user stopped recording), `waiting` (no target), `starting`
/// (engine spawning / encoder warming up / restarting), `recording` (NVENC
/// running for this target), `degraded` (recording, from CPU copies after the GPU
/// path was refused), `recovering` (the engine is rebuilding its encoder; the buffer
/// is kept and saveable), `error` (engine gave up, or its own encoder recovery is
/// spent: `record_recover` then restarts it).
pub fn session_state(wanted: &EngineTarget, health: EngineHealth, encoder: Option<&str>, fresh: bool, paused: bool) -> &'static str {
    if paused {
        return "paused";
    }
    if health == EngineHealth::GivenUp {
        return "error";
    }
    if *wanted == EngineTarget::Off {
        return "waiting";
    }
    match health {
        EngineHealth::Running { pid } if pid != 0 && fresh => match encoder {
            Some("failed") => "error",
            Some("recovering") => "recovering",
            Some("degraded") => "degraded",
            Some("starting") | Some("off") | None => "starting",
            Some(_) => "recording",
        },
        _ => "starting",
    }
}

pub fn target_json(wanted: &EngineTarget) -> Value {
    match wanted {
        EngineTarget::Window(t) => json!({"name": t.name, "exe": t.exe, "pid": t.pid}),
        EngineTarget::Desktop => json!({"name": DESKTOP_NAME, "exe": null, "pid": null}),
        EngineTarget::Off => Value::Null,
    }
}

fn target_name(wanted: &EngineTarget) -> &str {
    match wanted {
        EngineTarget::Window(t) => &t.name,
        EngineTarget::Desktop => DESKTOP_NAME,
        EngineTarget::Off => "",
    }
}

/// `draining` wins over `waiting` / a desktop start while the closed game's
/// engine still runs (a pause or a crash ends the drain: no ring is left).
fn state_with_drain(i: &StatusInputs) -> &'static str {
    let running = matches!(i.health, EngineHealth::Running { pid } if pid != 0);
    if i.drain.is_some() && running && !i.paused {
        return "draining";
    }
    session_state(i.wanted, i.health, i.encoder, i.fresh_engine, i.paused)
}

pub fn status_json(i: &StatusInputs) -> Value {
    let cfg = i.cfg;
    let rec = &cfg.recording;
    let state = state_with_drain(i);
    let drain = if state == "draining" { i.drain.clone().unwrap_or(Value::Null) } else { Value::Null };
    json!({
        "type": "status",
        "mode": "record",
        "state": state,
        "paused": i.paused,
        "target": target_json(i.wanted),
        "engine_health": health_summary(i.health),
        "auto_record": rec.auto_record,
        "save_hotkey": rec.save_hotkey,
        "mic_hotkey": rec.mic_hotkey,
        "save_seconds": rec.buffer_secs,
        "save_hotkeys": rec.save_hotkeys,
        "bookmark_hotkey": rec.bookmark_hotkey,
        "screenshot_hotkey": rec.screenshot_hotkey,
        "record_hotkey": rec.record_hotkey,
        "stats_hotkey": rec.stats_hotkey,
        "overlay_hotkey": rec.overlay_hotkey,
        "editor_hotkey": rec.editor_hotkey,
        "hotkey_problems": i.hotkey_problems,
        "mic_muted": i.mic_muted,
        "long_recording": i.long_recording,
        "pending_restart": i.pending_restart,
        "overlay": {
            "enabled": cfg.overlay.enabled,
            "corner": cfg.overlay.corner,
            "running": i.overlay_running,
            "stats_style": cfg.overlay.stats_style,
        },
        "overlay_stats": cfg.overlay.stats,
        "fps_source": i.fps_source,
        "autostart": cfg.startup.autostart,
        "storage": {"cap_gb": cfg.storage.cap_gb},
        "hdr": i.hdr.to_json(),
        "drain": drain,
    })
}

/// The tray status line for a state.
pub fn tray_line(state: &str, wanted: &EngineTarget) -> String {
    let name = target_name(wanted);
    match state {
        "recording" => format!("Recording {name}"),
        "degraded" => format!("Recording {name} (CPU copies)"),
        "recovering" => format!("Encoder recovering: {name} (the buffer is kept)"),
        "starting" => format!("Starting capture: {name}"),
        "paused" => "Recording paused".into(),
        "draining" => "Game closed: last clip kept".into(),
        "error" => "Recorder error (see recall-hdr.log)".into(),
        _ => "Waiting for a game".into(),
    }
}

pub fn error_event(message: &str) -> String {
    json!({"type": "error", "message": message}).to_string()
}

pub fn config_event(cfg: &Config) -> String {
    json!({"type": "config", "config": crate::record_config::config_json(cfg)}).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::record_target::Target;

    fn hdr() -> HdrSnapshot {
        HdrSnapshot { available: false, display: None, hdr_on: false, manual_pin: false, auto: false, session_pid: None }
    }

    fn game() -> EngineTarget {
        EngineTarget::Window(Target { hwnd: 7, pid: 42, name: "VALORANT".into(), exe: "VALORANT-Win64-Shipping.exe".into() })
    }

    #[test]
    fn state_machine_covers_every_phase() {
        let run = EngineHealth::Running { pid: 9 };
        assert_eq!(session_state(&EngineTarget::Off, EngineHealth::Stopped, None, false, false), "waiting");
        assert_eq!(session_state(&EngineTarget::Off, run, Some("h264_nvenc"), true, false), "waiting", "lingering engine, no game");
        assert_eq!(session_state(&game(), EngineHealth::Running { pid: 0 }, None, false, false), "starting");
        assert_eq!(session_state(&game(), run, Some("h264_nvenc"), false, false), "starting", "old engine's status is not ours");
        assert_eq!(session_state(&game(), run, Some("starting"), true, false), "starting");
        assert_eq!(session_state(&game(), run, Some("h264_nvenc"), true, false), "recording");
        assert_eq!(session_state(&game(), EngineHealth::Restarting { attempt: 2 }, None, true, false), "starting");
        assert_eq!(session_state(&game(), run, Some("failed"), true, false), "error");
        assert_eq!(session_state(&EngineTarget::Off, EngineHealth::GivenUp, None, false, false), "error");
        assert_eq!(session_state(&EngineTarget::Off, EngineHealth::Stopped, None, false, true), "paused");
        assert_eq!(session_state(&EngineTarget::Off, EngineHealth::GivenUp, None, false, true), "paused", "pause wins");
        assert_eq!(tray_line("paused", &EngineTarget::Off), "Recording paused");
    }

    #[test]
    fn an_engine_recovering_or_on_cpu_copies_is_its_own_state_not_recording() {
        let run = EngineHealth::Running { pid: 9 };
        assert_eq!(session_state(&game(), run, Some("recovering"), true, false), "recovering");
        assert_eq!(session_state(&game(), run, Some("degraded"), true, false), "degraded");
        assert_eq!(tray_line("recovering", &game()), "Encoder recovering: VALORANT (the buffer is kept)");
        assert_eq!(tray_line("degraded", &game()), "Recording VALORANT (CPU copies)");
    }

    #[test]
    fn status_json_has_the_exact_wire_shape() {
        let cfg = Config::default();
        let snap = hdr();
        let v = status_json(&StatusInputs {
            wanted: &game(),
            health: EngineHealth::Running { pid: 9 },
            encoder: Some("h264_nvenc"),
            fresh_engine: true,
            cfg: &cfg,
            mic_muted: true,
            overlay_running: false,
            fps_source: "off",
            hdr: &snap,
            paused: false,
            long_recording: json!({"active":true,"path":"E:\\c\\G__rec.mp4","started_secs":1790000000.5}),
            pending_restart: true,
            drain: None,
            hotkey_problems: &[],
        });
        assert_eq!(
            v,
            json!({
                "type":"status","mode":"record","state":"recording","paused":false,
                "target":{"name":"VALORANT","exe":"VALORANT-Win64-Shipping.exe","pid":42},
                "engine_health":"running","auto_record":true,
                "save_hotkey":"Alt+N","mic_hotkey":"Alt+M","save_seconds":120,
                "save_hotkeys":[{"hotkey":"Alt+N","seconds":0}],
                "bookmark_hotkey":"Alt+B","screenshot_hotkey":"Alt+S","record_hotkey":"Alt+L","stats_hotkey":"Alt+P","overlay_hotkey":"Alt+O","editor_hotkey":"Alt+U","hotkey_problems":[],
                "mic_muted":true,
                "long_recording":{"active":true,"path":"E:\\c\\G__rec.mp4","started_secs":1790000000.5},
                "pending_restart":true,
                "overlay":{"enabled":false,"corner":"dynamic","running":false,"stats_style":"minimal"},
                "overlay_stats":false,"fps_source":"off",
                "autostart":false,
                "storage":{"cap_gb":0},
                "hdr":{"available":false,"display":null,"hdr_on":false,"manual_pin":false,"auto":false,"shelved":true},
                "drain":null
            })
        );
    }

    #[test]
    fn stats_fields_follow_the_config_and_the_fps_source() {
        let mut cfg = Config::default();
        cfg.overlay.stats = true;
        let snap = hdr();
        let v = status_json(&StatusInputs {
            wanted: &EngineTarget::Off,
            health: EngineHealth::Stopped,
            encoder: None,
            fresh_engine: false,
            cfg: &cfg,
            mic_muted: false,
            overlay_running: true,
            fps_source: "no_permission",
            hdr: &snap,
            paused: false,
            long_recording: json!({"active":false,"path":null,"started_secs":null}),
            pending_restart: false,
            drain: None,
            hotkey_problems: &[],
        });
        assert_eq!(v["overlay_stats"], true);
        assert_eq!(v["fps_source"], "no_permission");
        assert!(v.get("rtss").is_none(), "the RTSS field is gone");
        assert_eq!(v["overlay"], json!({"enabled":false,"corner":"dynamic","running":true,"stats_style":"minimal"}));
    }

    #[test]
    fn a_running_drain_reports_draining_with_its_end_and_no_target() {
        let cfg = Config::default();
        let snap = hdr();
        let drain = json!({"name":"sobosuba","total_secs":120,"ends_unix_ms":1793869759953u64});
        let inputs = |health, paused| StatusInputs {
            wanted: &EngineTarget::Off,
            health,
            encoder: Some("h264_nvenc"),
            fresh_engine: true,
            cfg: &cfg,
            mic_muted: false,
            overlay_running: false,
            fps_source: "off",
            hdr: &snap,
            paused,
            long_recording: json!({"active":false,"path":null,"started_secs":null}),
            pending_restart: false,
            drain: Some(drain.clone()),
            hotkey_problems: &[],
        };
        let v = status_json(&inputs(EngineHealth::Running { pid: 9 }, false));
        assert_eq!(v["state"], "draining");
        assert_eq!(v["drain"], drain);
        assert_eq!(v["target"], Value::Null, "the gate and the island see no game");
        let stopped = status_json(&inputs(EngineHealth::Stopped, false));
        assert_eq!((stopped["state"].as_str(), &stopped["drain"]), (Some("waiting"), &Value::Null), "no engine, no clip");
        assert_eq!(status_json(&inputs(EngineHealth::Running { pid: 9 }, true))["state"], "paused");
        assert_eq!(tray_line("draining", &EngineTarget::Off), "Game closed: last clip kept");
    }

    #[test]
    fn desktop_and_idle_targets_serialize() {
        assert_eq!(target_json(&EngineTarget::Desktop), json!({"name":"Desktop","exe":null,"pid":null}));
        assert_eq!(target_json(&EngineTarget::Off), Value::Null);
        assert_eq!(tray_line("recording", &game()), "Recording VALORANT");
        assert_eq!(tray_line("waiting", &EngineTarget::Off), "Waiting for a game");
        assert_eq!(tray_line("starting", &EngineTarget::Desktop), "Starting capture: Desktop");
    }

    #[test]
    fn error_and_config_events_are_valid_json() {
        let e: Value = serde_json::from_str(&error_event(r#"bad "quote" \ here"#)).unwrap();
        assert_eq!(e, json!({"type":"error","message":r#"bad "quote" \ here"#}));
        let c: Value = serde_json::from_str(&config_event(&Config::default())).unwrap();
        assert_eq!(c["type"], "config");
        assert_eq!(c["config"]["recording"]["buffer_secs"], 120);
    }
}
