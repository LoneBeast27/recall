//! `Controller::apply_config_patch`: validate, persist and apply a config patch (split from
//! record_cmds.rs, which was over 500 lines).
use super::*;

impl Controller {
    /// Validate, persist and apply a config patch. `client` gets errors; the
    /// new config + status are broadcast. Returns whether it applied.
    pub(crate) fn apply_config_patch(&mut self, patch: &Value, client: Option<u64>) -> bool {
        self.apply_config_change(client, |_| Some(patch.clone()))
    }

    /// Build per-app changes from the current config, including any settled hand edit.
    pub(crate) fn apply_config_change(
        &mut self,
        client: Option<u64>,
        make_patch: impl FnOnce(&crate::config::Config) -> Option<Value>,
    ) -> bool {
        // A hand edit made while running is the base, not overwritten (`config_watch`).
        let mut broken_edit = None;
        match crate::config_watch::base_for_patch(&self.store, &mut self.config_stamp, std::time::SystemTime::now()) {
            Base::Memory => {}
            Base::Edited(edited) => {
                self.file_cfg = *edited;
                self.config_repairs = crate::config_repairs::ConfigRepairs::after_load(self.store.path());
            }
            Base::Broken(line) => broken_edit = Some(line),
            Base::Busy => {
                self.reply_error(client, "config.toml was changed on disk a moment ago; settings not saved, try again");
                return false;
            }
        }
        let patch = make_patch(&self.file_cfg).unwrap_or_else(|| json!({}));
        let out = match apply_patch(&self.file_cfg, &patch) {
            Ok(o) => o,
            Err(e) => {
                self.reply_error(client, &e);
                return false;
            }
        };
        // A patched key also retires its startup env override.
        let mut overrides = self.overrides.clone();
        overrides.forget(&patch);
        let effective = overrides.apply(&out.config);
        if out.changed.is_empty() && effective == self.cfg {
            // Nothing to persist (keeps the user's hand-written file untouched).
            if let Some(line) = crate::config_watch::patch_notice(broken_edit.as_deref(), false) {
                self.reply_error(client, &line);
            }
            if let Some(c) = client {
                self.hub.event_to(c, "session", &config_event(&self.cfg, &self.overrides, &self.config_repairs));
            }
            return true;
        }
        if !out.changed.is_empty() {
            if let Err(e) = self.store.save(&out.config) {
                self.reply_error(client, &format!("config save failed: {e}"));
                return false;
            }
            self.config_stamp = crate::config_watch::FileStamp::of(self.store.path());
            log!("record: config updated ({})", out.changed.join(", "));
        }
        if let Some(line) = crate::config_watch::patch_notice(broken_edit.as_deref(), !out.changed.is_empty()) {
            self.reply_error(client, &line);
        }
        let old = std::mem::replace(&mut self.cfg, effective);
        self.file_cfg = out.config;
        self.overrides = overrides;
        self.apply_config_effects(&old, client);
        self.hub.event("session", &config_event(&self.cfg, &self.overrides, &self.config_repairs));
        self.publish_status();
        true
    }
}
