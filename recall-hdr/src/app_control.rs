//! The legacy tray's hub control-pipe commands and the structured HDR status
//! it reports to hub clients (a second `impl App` block, pump thread only).

use crate::control::PipeHub;
use crate::hdr_exec;
use crate::log;
use crate::supervisor::{EngineHealth, Supervisor};

use super::{health_summary, App};

impl App {
    /// Execute one hub command line ON THE PUMP THREAD (display state is
    /// single-threaded by design; the pipe threads only queue lines).
    pub(super) fn handle_control_cmd(&mut self, line: &str, supervisor: &Supervisor, hub: &std::sync::Arc<PipeHub>) {
        let Ok(val) = serde_json::from_str::<serde_json::Value>(line) else {
            return;
        };
        match val.get("cmd").and_then(|c| c.as_str()) {
            Some("save_clip") => {
                log!("app: control save_clip -> engine");
                let seconds = val.get("seconds").and_then(|s| s.as_u64());
                if let Some(reason) = crate::record_dispatch::legacy_save(|line| supervisor.send_engine_line(line), seconds) {
                    log!("app: control save_clip not sent: {reason}");
                    hub.event("engine", &crate::record_refusal::refusal_event("save_clip", reason).to_string());
                    crate::supervisor::beep_failure();
                }
            }
            Some("export_clip") => {
                if matches!(supervisor.health(), EngineHealth::Running { .. }) {
                    if let Some(reason) = crate::record_dispatch::refusal(&crate::record_dispatch::command_effects(supervisor.send_engine_line(&val.to_string()))) {
                        hub.event("engine", &serde_json::json!({
                            "type": "export_failed", "source": val.get("path").and_then(|v| v.as_str()).unwrap_or(""),
                            "reason": reason
                        }).to_string());
                        crate::supervisor::beep_failure();
                    }
                } else {
                    hub.event("engine", &serde_json::json!({
                        "type": "export_failed", "source": val.get("path").and_then(|v| v.as_str()).unwrap_or(""),
                        "reason": "recording engine is offline; start Recall before exporting"
                    }).to_string());
                }
            }
            Some("toggle_hdr") => {
                log!("app: control toggle_hdr");
                self.toggle_hdr_now();
            }
            Some("set_mode") => {
                if let Some(v) = val.get("value").and_then(|v| v.as_str()) {
                    if matches!(v, "auto" | "sdr" | "hdr") {
                        let queued = supervisor.send_engine_line(&format!(r#"{{"cmd":"set_mode","value":"{v}"}}"#));
                        // The legacy app has no notice path or client to reply to: logged + beep.
                        let refused = crate::record_dispatch::refusal(&crate::record_dispatch::pipe_setting_effects(queued));
                        if let Some(reason) = refused.filter(|_| matches!(supervisor.health(), EngineHealth::Running { .. })) {
                            log!("app: control set_mode not sent: {reason}");
                            crate::supervisor::beep_failure();
                        }
                    }
                }
            }
            Some("status") => {
                // On-demand snapshot for a client that just connected.
                hub.event("hdr", &self.hdr_status_json(supervisor));
                for line in supervisor.cached_engine_events() {
                    hub.event("engine", &line);
                }
            }
            Some("set_ac_profile") => {
                let variant = val.get("variant").and_then(|v| v.as_str()).unwrap_or("");
                let profile = match hdr_exec::ac_profile_file(&self.cfg.display, variant) {
                    Ok(profile) => profile,
                    Err(e) => {
                        log!("app: control set_ac_profile -- {e}");
                        return;
                    }
                };
                let target = self.cfg.display.target_friendly_name.clone();
                let result = crate::wcs::set_advanced_color_default(&target, &profile).map(|()| profile.as_str());
                match &result {
                    Ok(_) => log!("app: control set_ac_profile -> {profile} (next HDR entry)"),
                    Err(e) => log!("app: control set_ac_profile FAILED: {e}"),
                }
                hub.event("hdr", &hdr_exec::ac_profile_event(&result));
            }
            _ => log!("app: control -- unknown command line ignored"),
        }
    }

    /// Structured HDR/daemon state for hub clients.
    pub(super) fn hdr_status_json(&self, supervisor: &Supervisor) -> String {
        serde_json::json!({
            "type": "status",
            "target": self.exec.target().name(),
            "hdr": self.reconciler.actual().hdr,
            "manual_pin": self.reconciler.manual_pinned(),
            "session_pid": self.reconciler.session_pid(),
            "auto": self.auto_enabled,
            "engine_health": health_summary(supervisor.health()),
        })
        .to_string()
    }
}
