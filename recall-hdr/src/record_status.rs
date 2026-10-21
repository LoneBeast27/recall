//! The record mode's `mod:"session"` status event and tray line, built from
//! plain inputs so the exact wire shape is unit-tested.
//!
//! ```json
//! {"type":"status","mode":"record","state":"waiting|starting|recording|error",
//!  "target":{"name":S,"exe":S|null,"pid":N|null}|null,
//!  "engine_health":"running|restarting|off|stopped","auto_record":B,
//!  "save_hotkey":S,"mic_hotkey":S,"save_seconds":N,"mic_muted":B,
//!  "overlay":{"enabled":B,"corner":S,"running":B},"autostart":B,
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
    pub hdr: &'a HdrSnapshot,
}

/// `waiting` (no target), `starting` (engine spawning / encoder warming up /
/// restarting), `recording` (NVENC running for this target), `error` (engine
/// gave up or the encoder failed).
pub fn session_state(wanted: &EngineTarget, health: EngineHealth, encoder: Option<&str>, fresh: bool) -> &'static str {
    if health == EngineHealth::GivenUp {
        return "error";
    }
    if *wanted == EngineTarget::Off {
        return "waiting";
    }
    match health {
        EngineHealth::Running { pid } if pid != 0 && fresh => match encoder {
            Some("failed") => "error",
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

pub fn status_json(i: &StatusInputs) -> Value {
    let cfg = i.cfg;
    json!({
        "type": "status",
        "mode": "record",
        "state": session_state(i.wanted, i.health, i.encoder, i.fresh_engine),
        "target": target_json(i.wanted),
        "engine_health": health_summary(i.health),
        "auto_record": cfg.recording.auto_record,
        "save_hotkey": cfg.recording.save_hotkey,
        "mic_hotkey": cfg.recording.mic_hotkey,
        "save_seconds": cfg.recording.buffer_secs,
        "mic_muted": i.mic_muted,
        "overlay": {
            "enabled": cfg.overlay.enabled,
            "corner": cfg.overlay.corner,
            "running": i.overlay_running,
        },
        "autostart": cfg.startup.autostart,
        "hdr": i.hdr.to_json(),
    })
}

/// The tray status line for a state.
pub fn tray_line(state: &str, wanted: &EngineTarget) -> String {
    let name = target_name(wanted);
    match state {
        "recording" => format!("Recording {name}"),
        "starting" => format!("Starting capture: {name}"),
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
        assert_eq!(session_state(&EngineTarget::Off, EngineHealth::Stopped, None, false), "waiting");
        assert_eq!(session_state(&EngineTarget::Off, run, Some("h264_nvenc"), true), "waiting", "lingering engine, no game");
        assert_eq!(session_state(&game(), EngineHealth::Running { pid: 0 }, None, false), "starting");
        assert_eq!(session_state(&game(), run, Some("h264_nvenc"), false), "starting", "old engine's status is not ours");
        assert_eq!(session_state(&game(), run, Some("starting"), true), "starting");
        assert_eq!(session_state(&game(), run, Some("h264_nvenc"), true), "recording");
        assert_eq!(session_state(&game(), EngineHealth::Restarting { attempt: 2 }, None, true), "starting");
        assert_eq!(session_state(&game(), run, Some("failed"), true), "error");
        assert_eq!(session_state(&EngineTarget::Off, EngineHealth::GivenUp, None, false), "error");
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
            hdr: &snap,
        });
        assert_eq!(
            v,
            json!({
                "type":"status","mode":"record","state":"recording",
                "target":{"name":"VALORANT","exe":"VALORANT-Win64-Shipping.exe","pid":42},
                "engine_health":"running","auto_record":true,
                "save_hotkey":"Alt+N","mic_hotkey":"Alt+M","save_seconds":120,"mic_muted":true,
                "overlay":{"enabled":false,"corner":"top-right","running":false},
                "autostart":true,
                "hdr":{"available":false,"display":null,"hdr_on":false,"manual_pin":false,"auto":false}
            })
        );
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
