//! `set_config` on the record controller (split out of `record_cmds`, file
//! size; behaviour unchanged): validate, persist and apply a config patch,
//! then react to what changed.

use serde_json::Value;

use crate::config::{sync_autostart, RECORD_RUN_ARG};
use crate::log;
use crate::record_app::{apply_affinity, Controller};
use crate::record_config::{apply_patch, parse_cpu_mask};
use crate::record_dispatch::{pipe_setting_effects, Sent};
use crate::record_engine::engine_settings_changed;
use crate::record_status::config_event;

impl Controller {
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
        self.apply_config_effects(&old, client);
        self.hub.event("session", &config_event(&self.cfg));
        self.publish_status();
        true
    }

    /// React to the difference between the old and new effective config.
    fn apply_config_effects(&mut self, old: &crate::config::Config, client: Option<u64>) {
        let new = self.cfg.clone();
        self.recheck_registration(old);
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
            || o.editor_hotkey != n.editor_hotkey
            || old.hotkey.toggle_hdr != new.hotkey.toggle_hdr
        {
            self.rebind_hotkeys();
        }
        if o.cpu_mask != n.cpu_mask {
            if let Ok(mask) = parse_cpu_mask(&n.cpu_mask) {
                self.applied_mask = apply_affinity(mask);
            }
        }
        let live_change = crate::record_audio_sources::live_updates(&o.audio_sources, &n.audio_sources);
        if engine_settings_changed(o, n) {
            self.request_engine_restart();
        } else if self.engine_running() && !live_change.is_empty() && !self.supervisor.engine_takes_live_sources() {
            // An older engine ignores set_source: restart it so the new gain or name applies.
            log!("record: the engine cannot change audio sources live; restarting it");
            self.request_engine_restart();
        } else if self.engine_running() {
            // Gain and names reach the running engine's tracks: no restart, the buffer stays.
            // An engine started later reads them from its environment.
            let mut all_sent = Sent::Queued;
            for line in crate::record_audio_sources::live_updates(&o.audio_sources, &n.audio_sources) {
                log!("record: audio source {line}");
                all_sent = all_sent.then(self.supervisor.send_engine_line(&line));
            }
            // The config is saved either way; an engine started later reads it.
            self.setting_not_sent(pipe_setting_effects(all_sent), client, "Audio change not applied", "set_source");
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
}
