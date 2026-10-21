//! Control-pipe API of the unified record mode (the controller is the single
//! config writer). Commands, newline JSON on `\\.\pipe\recall-hdr-control`:
//!
//! | cmd | args | effect / reply |
//! |---|---|---|
//! | `status` | | session status + cached engine hello/status (+ legacy `hdr` status) to the caller |
//! | `quit` | | stop recording and exit |
//! | `save_clip` | `seconds?` | engine save (default = `recording.buffer_secs`); no engine -> `engine` `save_failed` |
//! | `export_clip` | unchanged engine shape | forwarded; engine off -> `engine` `export_failed` |
//! | `set_mic_mute` | `muted: bool` | enforced on this and every later engine |
//! | `toggle_mic_mute` | | flips the enforced state |
//! | `toggle_hdr` | | manual HDR pin (needs `hdr_auto` + the managed display) |
//! | `set_mode` | `value: auto|sdr|hdr` | engine tonemap selector, re-applied after restarts |
//! | `set_ac_profile` | `variant: punchy|accurate` | `hdr` `ac_profile` result event |
//! | `get_config` | | `config` event to the caller |
//! | `set_config` | `patch: {...}` | validated deep-merge, saved; `config` + status broadcast |
//!
//! Rejected commands get `{"type":"error","message":S}` (mod `session`) sent to
//! the requesting client only.

use serde_json::{json, Value};

use crate::config::{sync_autostart, RECORD_RUN_ARG};
use crate::control::Inbound;
use crate::hdr_exec;
use crate::log;
use crate::record_app::{apply_affinity, Controller};
use crate::record_config::{apply_patch, parse_cpu_mask};
use crate::record_status::{config_event, error_event, status_json, tray_line, StatusInputs};
use crate::record_engine::EngineTarget;

impl Controller {
    /// Execute one pipe command on the pump thread.
    pub(crate) fn handle_inbound(&mut self, inbound: Inbound) {
        let client = Some(inbound.client);
        let val: Value = match serde_json::from_str(&inbound.line) {
            Ok(v) => v,
            Err(e) => return self.reply_error(client, &format!("malformed command JSON: {e}")),
        };
        let cmd = val.get("cmd").and_then(Value::as_str).unwrap_or("").to_string();
        match cmd.as_str() {
            "status" => self.reply_status(inbound.client),
            "quit" => {
                log!("record: control quit");
                self.quit = true;
            }
            "save_clip" => match val.get("seconds") {
                None | Some(Value::Null) => self.save_clip(None),
                Some(v) => match v.as_u64().filter(|s| (1..=u32::MAX as u64).contains(s)) {
                    Some(s) => self.save_clip(Some(s as u32)),
                    None => self.reply_error(client, "save_clip.seconds must be a positive integer"),
                },
            },
            "export_clip" => self.export_clip(&val),
            "set_mic_mute" => match val.get("muted").and_then(Value::as_bool) {
                Some(muted) => self.set_mic(muted),
                None => self.reply_error(client, "set_mic_mute needs \"muted\": true|false"),
            },
            "toggle_mic_mute" => self.set_mic(!self.mic_muted),
            "toggle_hdr" => {
                if let Err(e) = self.hdr.toggle() {
                    self.reply_error(client, &e);
                }
            }
            "set_mode" => match val.get("value").and_then(Value::as_str) {
                Some(v @ ("auto" | "sdr" | "hdr")) => {
                    self.mode_select = Some(v.to_string());
                    self.supervisor.send_engine_line(&json!({"cmd": "set_mode", "value": v}).to_string());
                }
                _ => self.reply_error(client, "set_mode needs \"value\": auto|sdr|hdr"),
            },
            "set_ac_profile" => self.set_ac_profile(client, &val),
            "get_config" => self.hub.event_to(inbound.client, "session", &config_event(&self.cfg)),
            "set_config" => match val.get("patch") {
                Some(patch) => {
                    self.apply_config_patch(patch, client);
                }
                None => self.reply_error(client, "set_config needs a \"patch\" object"),
            },
            "" => self.reply_error(client, "missing \"cmd\""),
            other => self.reply_error(client, &format!("unknown command '{other}'")),
        }
    }

    pub(crate) fn reply_error(&self, client: Option<u64>, message: &str) {
        log!("record: rejected command: {message}");
        if let Some(c) = client {
            self.hub.event_to(c, "session", &error_event(message));
        }
    }

    /// Save the replay buffer (hotkey, tray, pipe). With no engine to ask, the
    /// failure is surfaced the way an engine failure is (overlay toast + beep).
    pub(crate) fn save_clip(&mut self, seconds: Option<u32>) {
        let secs = seconds.unwrap_or(self.cfg.recording.buffer_secs);
        if self.engine_running() {
            log!("record: save last {secs}s");
            self.supervisor.send_engine_line(&json!({"cmd": "save_clip", "seconds": secs}).to_string());
            return;
        }
        let reason = match self.wanted() {
            EngineTarget::Off => "Recall is waiting for a game; nothing is being recorded",
            _ => "the recording engine is still starting; try again in a moment",
        };
        log!("record: save refused -- {reason}");
        self.hub.event("engine", &json!({"type": "save_failed", "reason": reason}).to_string());
        crate::supervisor::beep_failure();
    }

    fn export_clip(&mut self, val: &Value) {
        if self.engine_running() {
            self.supervisor.send_engine_line(&val.to_string());
        } else {
            let source = val.get("path").and_then(Value::as_str).unwrap_or("");
            self.hub.event(
                "engine",
                &json!({"type": "export_failed", "source": source,
                    "reason": "recording engine is offline; exports run while Recall is recording"})
                .to_string(),
            );
        }
    }

    /// Enforce a microphone state now and on every future engine instance.
    pub(crate) fn set_mic(&mut self, muted: bool) {
        self.mic_muted = muted;
        log!("record: microphone {}", if muted { "muted" } else { "live" });
        self.supervisor.send_engine_line(&json!({"cmd": "set_mic_mute", "muted": muted}).to_string());
        self.publish_status();
    }

    fn set_ac_profile(&mut self, client: Option<u64>, val: &Value) {
        let variant = val.get("variant").and_then(Value::as_str).unwrap_or("");
        let Some(profile) = hdr_exec::ac_profile_file(variant) else {
            return self.reply_error(client, &format!("unknown set_ac_profile variant '{variant}' (expected punchy | accurate)"));
        };
        if let Err(e) = self.hdr.ready() {
            return self.reply_error(client, &e);
        }
        let result = self.hdr.set_ac_profile(&self.cfg, profile);
        self.hub.event("hdr", &hdr_exec::ac_profile_event(&result));
    }

    /// Validate, persist and apply a config patch. `client` gets errors; the
    /// new config + status are broadcast. Returns whether it applied.
    pub(crate) fn apply_config_patch(&mut self, patch: &Value, client: Option<u64>) -> bool {
        let out = match apply_patch(&self.file_cfg, patch) {
            Ok(o) => o,
            Err(e) => {
                self.reply_error(client, &e);
                return false;
            }
        };
        // A patched key also retires its startup env override.
        let mut overrides = self.overrides.clone();
        overrides.forget(patch);
        let effective = overrides.apply(&out.config);
        if out.changed.is_empty() && effective == self.cfg {
            // Nothing to persist (keeps the user's hand-written file untouched).
            if let Some(c) = client {
                self.hub.event_to(c, "session", &config_event(&self.cfg));
            }
            return true;
        }
        if !out.changed.is_empty() {
            if let Err(e) = self.store.save(&out.config) {
                self.reply_error(client, &format!("config save failed: {e}"));
                return false;
            }
            log!("record: config updated ({})", out.changed.join(", "));
        }
        let old = std::mem::replace(&mut self.cfg, effective);
        self.file_cfg = out.config;
        self.overrides = overrides;
        self.apply_config_effects(&old);
        self.hub.event("session", &config_event(&self.cfg));
        self.publish_status();
        true
    }

    /// React to the difference between the old and new effective config.
    fn apply_config_effects(&mut self, old: &crate::config::Config) {
        let new = self.cfg.clone();
        if old.startup.autostart != new.startup.autostart {
            match sync_autostart(new.startup.autostart, RECORD_RUN_ARG) {
                Ok(()) => log!("record: start with Windows -> {}", new.startup.autostart),
                Err(e) => self.reply_error(None, &format!("autostart registry update failed: {e}")),
            }
            self.tray.set_autostart_checked(new.startup.autostart);
        }
        if old.behavior.hdr_auto != new.behavior.hdr_auto {
            if new.behavior.hdr_auto {
                if let Err(e) = self.hdr.activate(&new) {
                    log!("record: HDR management could not start: {e}");
                }
            } else {
                self.hdr.deactivate(&new);
            }
        }
        let (o, n) = (&old.recording, &new.recording);
        if old.behavior.hdr_auto != new.behavior.hdr_auto
            || o.save_hotkey != n.save_hotkey
            || o.mic_hotkey != n.mic_hotkey
            || old.hotkey.toggle_hdr != new.hotkey.toggle_hdr
        {
            self.rebind_hotkeys();
        }
        if o.cpu_mask != n.cpu_mask {
            if let Ok(mask) = parse_cpu_mask(&n.cpu_mask) {
                self.applied_mask = apply_affinity(mask);
            }
        }
        if o.buffer_secs != n.buffer_secs || o.output_dir != n.output_dir || o.cpu_mask != n.cpu_mask {
            log!("record: engine settings changed; restarting the engine (the replay buffer restarts)");
            self.restart_engine_for_settings();
        }
        if old.behavior.sdr_white_nits != new.behavior.sdr_white_nits || old.behavior.debounce_ms != new.behavior.debounce_ms {
            log!("record: sdr_white_nits / debounce_ms apply the next time HDR management starts");
        }
        if old.display.target_friendly_name != new.display.target_friendly_name {
            log!("record: display.target_friendly_name applies after a restart");
        }
        self.overlay.sync(&new.overlay);
    }

    fn status_value(&mut self) -> Value {
        let wanted = self.wanted();
        let view = self.supervisor.engine_view();
        let hdr = self.hdr.snapshot(&self.cfg);
        status_json(&StatusInputs {
            wanted: &wanted,
            health: self.supervisor.health(),
            encoder: view.encoder.as_deref(),
            fresh_engine: self.supervisor.hello_count() > self.hello_at_retarget,
            cfg: &self.cfg,
            mic_muted: self.mic_muted,
            overlay_running: self.overlay.running(),
            hdr: &hdr,
        })
    }

    fn legacy_hdr_event(&self) -> Option<String> {
        let snap = self.hdr.snapshot(&self.cfg);
        snap.available
            .then(|| snap.legacy_event(crate::app::health_summary(self.supervisor.health())))
    }

    /// Broadcast the session status (and tray line) when anything changed.
    pub(crate) fn publish_status(&mut self) {
        let status = self.status_value();
        let line = status.to_string();
        if line != self.last_session {
            let state = status["state"].as_str().unwrap_or("waiting").to_string();
            self.tray.set_status(&tray_line(&state, &self.wanted()));
            self.hub.event("session", &line);
            self.last_session = line;
        }
        if let Some(hdr) = self.legacy_hdr_event() {
            if hdr != self.last_hdr_event {
                self.hub.event("hdr", &hdr);
                self.last_hdr_event = hdr;
            }
        }
    }

    /// Late-joiner / on-demand snapshot for one client.
    fn reply_status(&mut self, client: u64) {
        if !self.hdr.is_active() {
            self.hdr.refresh_live();
        }
        let status = self.status_value().to_string();
        self.hub.event_to(client, "session", &status);
        for line in self.supervisor.cached_engine_events() {
            self.hub.event_to(client, "engine", &line);
        }
        if let Some(hdr) = self.legacy_hdr_event() {
            self.hub.event_to(client, "hdr", &hdr);
        }
    }
}
