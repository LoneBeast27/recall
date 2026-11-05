//! Control-pipe API of the unified record mode (the controller is the single
//! config writer). Commands, newline JSON on `\\.\pipe\recall-hdr-control`:
//!
//! | cmd | args | effect / reply |
//! |---|---|---|
//! | `status` | | session status + cached engine hello/status (+ legacy `hdr` status) to the caller |
//! | `quit` | | stop recording and exit (the Recall app stays open) |
//! | `save_clip` | `seconds?` | engine save (default = `recording.buffer_secs`) + `session` `saving{seconds}` broadcast (the island's saving ring); no engine / paused -> `engine` `save_failed` |
//! | `bookmark` | | engine bookmark (marks land in the next saves' `.recall-marks.json`) |
//! | `screenshot` | | engine PNG -> `engine` `screenshot{path}`; refused -> `screenshot_failed` |
//! | `record_start` / `record_stop` / `record_toggle` | | manual long recording -> `engine` `recording_started{path}` / `recording_stopped{path,seconds,bytes}` / `recording_failed{reason,path}` |
//! | `pause` / `resume` / `toggle_pause` | | stop the engine and ignore games until resume (runtime only); status `state:"paused"` |
//! | `storage_clean` | | run the storage cap now -> `session` `storage_cleaned{removed,freed_bytes,through,used_bytes,cap_bytes}` (+ `storage_warning` at 80/90/100%) |
//! | `storage_preview` | `cap_gb` | dry run, nothing moves -> caller only: `session` `storage_preview{cap_gb,used_bytes,removed,freed_bytes,through}` |
//! | `export_clip` | unchanged engine shape | forwarded; engine off -> `engine` `export_failed` |
//! | `set_mic_mute` | `muted: bool` | enforced on this and every later engine |
//! | `toggle_mic_mute` | | flips the enforced state |
//! | `toggle_hdr` | | manual HDR pin (needs `hdr_auto` + the managed display) |
//! | `toggle_stats` | | flips `overlay.stats` (saved; tick); `config` + status broadcast |
//! | `set_mode` | `value: auto|sdr|hdr` | engine tonemap selector, re-applied after restarts |
//! | `set_ac_profile` | `variant: punchy|accurate` | `hdr` `ac_profile` result event |
//! | `get_config` | | `config` event to the caller |
//! | `set_config` | `patch: {...}` | validated deep-merge, saved; `config` + status broadcast |
//!
//! Rejected commands get `{"type":"error","message":S}` (mod `session`) sent to
//! the requesting client only. Recording changes also broadcast a `session`
//! `notice` (`record_notice`) while `overlay.enabled`.

use std::time::Instant;

use serde_json::{json, Value};

use crate::config::{sync_autostart, RECORD_RUN_ARG};
use crate::control::{Inbound, PipeHub};
use crate::hdr_exec;
use crate::log;
use crate::record_app::{apply_affinity, Controller};
use crate::record_config::{apply_patch, parse_cpu_mask};
use crate::record_notice::{notice_json, Situation};
use crate::record_status::{config_event, error_event, status_json, tray_line, StatusInputs};
use crate::record_engine::{capture_refusal, engine_settings_changed};
use crate::supervisor::EngineHealth;

/// `{"type":"saving","seconds":N}` (mod `session`): a save the controller
/// accepted and handed to the engine. The island spins its shutter ring
/// until the engine's `saved` / `save_failed` (or 30 s pass).
pub fn saving_event(seconds: u32) -> String {
    json!({"type": "saving", "seconds": seconds}).to_string()
}

/// Every accepted save goes through here (pipe `save_clip`, the save
/// hotkeys): the engine command, then the broadcast.
/// `at_last_frame` (a drain: the game closed) ends the clip at the game's last
/// frame instead of holding that frame up to now.
pub(crate) fn send_save(hub: &PipeHub, send_engine: impl FnOnce(&str), seconds: u32, at_last_frame: bool) {
    let mut cmd = json!({"cmd": "save_clip", "seconds": seconds});
    if at_last_frame {
        cmd["at_last_frame"] = Value::Bool(true);
    }
    send_engine(&cmd.to_string());
    hub.event("session", &saving_event(seconds));
}

/// The `set_config` patch that turns the performance-stats island on or off.
pub fn stats_patch(on: bool) -> Value {
    json!({"overlay": {"stats": on}})
}

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
                self.quit = Some(crate::record_quit::QuitReason::ControlPipe);
            }
            "save_clip" => match val.get("seconds") {
                None | Some(Value::Null) => self.save_clip(None),
                Some(v) => match v.as_u64().filter(|s| (1..=u32::MAX as u64).contains(s)) {
                    Some(s) => self.save_clip(Some(s as u32)),
                    None => self.reply_error(client, "save_clip.seconds must be a positive integer"),
                },
            },
            "export_clip" => self.export_clip(&val),
            "bookmark" => self.engine_command("bookmark", client),
            "screenshot" => self.engine_command("screenshot", client),
            // The island's Dynamic spot: quiet when nothing captures (no failure cue).
            "hud_probe" => {
                if self.engine_running() && !self.paused {
                    log!("record: engine hud_probe");
                    self.supervisor.send_engine_line(r#"{"cmd":"hud_probe"}"#);
                } else {
                    self.reply_error(client, "hud_probe: nothing is being captured");
                }
            }
            "record_start" => self.engine_command("record_start", client),
            "record_stop" => self.engine_command("record_stop", client),
            "record_toggle" => self.engine_command("record_toggle", client),
            "pause" => self.set_paused(true),
            "resume" => self.set_paused(false),
            "toggle_pause" => self.set_paused(!self.paused),
            "storage_clean" => {
                let (dir, cap) = (self.cfg.recording.output_dir.clone(), self.cfg.storage.cap_gb);
                if cap == 0 {
                    self.reply_error(client, "storage.cap_gb is 0 (auto-clean off)");
                } else {
                    self.janitor.run_now(&dir, cap, std::time::Instant::now());
                }
            }
            "storage_preview" => match (val.get("cap_gb").and_then(Value::as_u64), client) {
                (Some(cap), Some(c)) if cap >= u64::from(crate::record_config::STORAGE_CAP_MIN_GB) && cap <= u64::from(u32::MAX) => {
                    self.janitor.preview(&self.cfg.recording.output_dir.clone(), cap as u32, c)
                }
                (_, Some(_)) => self.reply_error(client, &format!("storage_preview needs \"cap_gb\" of at least {} GiB", crate::record_config::STORAGE_CAP_MIN_GB)),
                (_, None) => {}
            },
            "set_mic_mute" => match val.get("muted").and_then(Value::as_bool) {
                Some(muted) => self.set_mic(muted),
                None => self.reply_error(client, "set_mic_mute needs \"muted\": true|false"),
            },
            "toggle_mic_mute" => self.set_mic(!self.mic_muted),
            "toggle_stats" => self.toggle_stats(client),
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

    /// Save the replay buffer (hotkeys, pipe). Paused or with no engine
    /// to ask, the failure is surfaced the way an engine failure is (overlay
    /// toast + beep).
    pub(crate) fn save_clip(&mut self, seconds: Option<u32>) {
        let secs = seconds.unwrap_or(self.cfg.recording.buffer_secs);
        if let Some(reason) = capture_refusal(self.paused, self.engine_running(), &self.wanted()) {
            return self.refuse_capture("save_clip", reason);
        }
        let draining = self.planner.drain().is_some();
        log!("record: save last {secs}s{}", if draining { " (game closed: ends at its last frame)" } else { "" });
        self.activity.save_sent(std::time::Instant::now());
        send_save(&self.hub, |line| self.supervisor.send_engine_line(line), secs, draining);
        // Heard at once; the chime follows when the clip is on disk (seconds for a long save to a hard drive).
        crate::cues::tick();
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

    /// Flip the performance-stats island (pipe `toggle_stats`, its hotkey). Saved
    /// like any `set_config`, so it survives restarts; the effects (tick,
    /// overlay process) follow in `apply_config_effects`.
    pub(crate) fn toggle_stats(&mut self, client: Option<u64>) {
        let on = !self.cfg.overlay.stats;
        self.apply_config_patch(&stats_patch(on), client);
    }

    /// Enforce a microphone state now and on every future engine instance.
    /// With no engine to echo `mic_muted`, the controller sends it itself so
    /// the shortcut still gets its pop-up.
    pub(crate) fn set_mic(&mut self, muted: bool) {
        self.mic_muted = muted;
        log!("record: microphone {}", if muted { "muted" } else { "live" });
        self.supervisor.send_engine_line(&json!({"cmd": "set_mic_mute", "muted": muted}).to_string());
        if !self.engine_running() {
            self.hub.event("engine", &json!({"type": "mic_muted", "muted": muted}).to_string());
        }
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
            || o.save_hotkeys != n.save_hotkeys
            || o.mic_hotkey != n.mic_hotkey
            || o.bookmark_hotkey != n.bookmark_hotkey
            || o.screenshot_hotkey != n.screenshot_hotkey
            || o.record_hotkey != n.record_hotkey
            || o.stats_hotkey != n.stats_hotkey
            || o.overlay_hotkey != n.overlay_hotkey
            || old.hotkey.toggle_hdr != new.hotkey.toggle_hdr
        {
            self.rebind_hotkeys();
        }
        if o.cpu_mask != n.cpu_mask {
            if let Ok(mask) = parse_cpu_mask(&n.cpu_mask) {
                self.applied_mask = apply_affinity(mask);
            }
        }
        if engine_settings_changed(o, n) {
            self.request_engine_restart();
        }
        if old.storage.cap_gb != new.storage.cap_gb || o.output_dir != n.output_dir {
            log!("record: storage cap -> {} GiB", new.storage.cap_gb);
            self.janitor.run_now(&n.output_dir, new.storage.cap_gb, std::time::Instant::now());
        }
        if old.behavior.sdr_white_nits != new.behavior.sdr_white_nits || old.behavior.debounce_ms != new.behavior.debounce_ms {
            log!("record: sdr_white_nits / debounce_ms apply the next time HDR management starts");
        }
        if old.display.target_friendly_name != new.display.target_friendly_name {
            log!("record: display.target_friendly_name applies after a restart");
        }
        if old.overlay.stats != new.overlay.stats {
            log!("record: performance stats {}", if new.overlay.stats { "on" } else { "off" });
            crate::cues::tick();
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
            fps_source: crate::frame_timing::fps_source(self.cfg.overlay.stats, self.can_trace).as_str(),
            hdr: &hdr,
            paused: self.paused,
            long_recording: self.activity.long_recording_json(),
            // Settings restart or a held retarget: both wait for idle.
            pending_restart: self.pending_restart || self.retarget_held,
            drain: self.drain_clock.status(self.planner.drain(), Instant::now()),
            hotkey_problems: &self.hotkey_problems,
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
            self.tray.set_paused(self.paused);
            self.tray.set_status(&tray_line(&state, &self.wanted()));
            self.hub.event("session", &line);
            self.last_session = line;
            self.announce(&state);
        }
        if let Some(hdr) = self.legacy_hdr_event() {
            if hdr != self.last_hdr_event {
                self.hub.event("hdr", &hdr);
                self.last_hdr_event = hdr;
            }
        }
    }

    /// Recall Island notice + cue for a recording change. The tracker always
    /// follows the state; with notices off nothing is sent or heard.
    fn announce(&mut self, state: &str) {
        let gave_up = self.supervisor.health() == EngineHealth::GivenUp;
        let next = match (state, self.planner.drain()) {
            ("draining", Some((_, _, len))) => Some(Situation::Draining { secs: len.as_secs_f32().round() as u64 }),
            _ => Situation::of(state, &self.wanted(), gave_up),
        };
        let Some(notice) = self.notices.observe(next, Instant::now()) else { return };
        if !self.cfg.overlay.enabled {
            return;
        }
        log!("record: notice {}: {} {}", notice.kind(), notice.title(), notice.detail());
        self.hub.event("session", &notice_json(&notice));
        if notice.is_failure() {
            crate::cues::failed();
        } else {
            crate::cues::tick();
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

#[cfg(test)]
mod save_tests {
    use super::*;
    use std::io::Write;
    use std::sync::mpsc::channel;
    use std::sync::{Arc, Mutex};
    use std::time::{Duration, Instant};

    #[derive(Clone, Default)]
    struct Sink(Arc<Mutex<Vec<u8>>>);
    impl Write for Sink {
        fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(b);
            Ok(b.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn an_accepted_save_goes_to_the_engine_then_tells_every_client_it_is_saving() {
        let (tx, _rx) = channel();
        let hub = PipeHub::detached(tx);
        let (a, b) = (Sink::default(), Sink::default());
        hub.add_client(Box::new(a.clone()));
        hub.add_client(Box::new(b.clone()));
        let mut sent = Vec::new();
        send_save(&hub, |line| sent.push(line.to_owned()), 30, false);
        assert_eq!(sent, vec![r#"{"cmd":"save_clip","seconds":30}"#.to_owned()], "one engine command");
        let deadline = Instant::now() + Duration::from_secs(5);
        let lines = |s: &Sink| String::from_utf8(s.0.lock().unwrap().clone()).unwrap();
        while (lines(&a).is_empty() || lines(&b).is_empty()) && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(5));
        }
        for sink in [&a, &b] {
            let text = lines(sink);
            let line: Value = serde_json::from_str(text.trim()).expect("one JSON line");
            assert_eq!(line, json!({"mod":"session","event":{"type":"saving","seconds":30}}));
        }
        let mut drained = Vec::new();
        send_save(&hub, |line| drained.push(line.to_owned()), 120, true);
        let v: Value = serde_json::from_str(&drained[0]).unwrap();
        assert_eq!(v, json!({"cmd":"save_clip","seconds":120,"at_last_frame":true}), "a drain save ends at the last frame");
    }
}
