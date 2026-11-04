//! Startup-only environment overrides for the record mode -- the
//! `Start-Recall.ps1` contract (`RECALL_OUT_DIR`, `RECALL_BUFFER_SECS`,
//! `RECALL_SAVE_HOTKEY`, `RECALL_MIC_HOTKEY`, `RECALL_CPU_MASK`,
//! `RECALL_OVERLAY`, `RECALL_OVERLAY_CORNER`). They are layered over
//! `config.toml` at startup and never written back to it.

use serde_json::{Map, Value};

use crate::config::Config;
use crate::log;
use crate::record_config::apply_patch;

/// Stored patch-shaped; a later `set_config` that touches a key drops its
/// override so a change made in the app always takes effect.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct EnvOverrides(Map<String, Value>);

impl EnvOverrides {
    pub fn from_env() -> Self {
        Self::from_lookup(|k| std::env::var(k).ok())
    }

    /// Build from an injectable lookup (tests pass a closure over a map).
    pub fn from_lookup(get: impl Fn(&str) -> Option<String>) -> Self {
        let mut o = EnvOverrides::default();
        let text = |k: &str| get(k).map(|v| v.trim().to_string()).filter(|v| !v.is_empty());
        for (var, key) in [
            ("RECALL_OUT_DIR", "output_dir"),
            ("RECALL_SAVE_HOTKEY", "save_hotkey"),
            ("RECALL_MIC_HOTKEY", "mic_hotkey"),
            ("RECALL_CPU_MASK", "cpu_mask"),
        ] {
            if let Some(v) = text(var) {
                o.set("recording", key, Value::String(v));
            }
        }
        if let Some(v) = text("RECALL_BUFFER_SECS") {
            match v.parse::<u32>() {
                Ok(n) => o.set("recording", "buffer_secs", Value::from(n)),
                Err(_) => log!("config: ignoring RECALL_BUFFER_SECS={v:?} (not a number)"),
            }
        }
        if let Some(v) = text("RECALL_OVERLAY") {
            let on = matches!(v.to_ascii_lowercase().as_str(), "1" | "true" | "on" | "yes");
            o.set("overlay", "enabled", Value::Bool(on));
        }
        if let Some(v) = text("RECALL_OVERLAY_CORNER") {
            o.set("overlay", "corner", Value::String(v));
        }
        o
    }

    fn set(&mut self, section: &str, key: &str, value: Value) {
        let entry = self.0.entry(section).or_insert_with(|| Value::Object(Map::new()));
        if let Value::Object(m) = entry {
            m.insert(key.to_string(), value);
        }
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// The effective config: `file` with these overrides on top. An override set
    /// that fails validation is logged and ignored (the file config wins).
    pub fn apply(&self, file: &Config) -> Config {
        if self.is_empty() {
            return file.clone();
        }
        match apply_patch(file, &Value::Object(self.0.clone())) {
            Ok(out) => out.config,
            Err(e) => {
                log!("config: environment overrides rejected ({e}); using config.toml values");
                file.clone()
            }
        }
    }

    /// Drop every override whose leaf key `patch` sets. Setting the save
    /// presets (`save_hotkeys`) also retires a `RECALL_SAVE_HOTKEY` override.
    pub fn forget(&mut self, patch: &Value) {
        let Value::Object(p) = patch else { return };
        for (section, keys) in p {
            let Some(Value::Object(sec)) = self.0.get_mut(section) else { continue };
            match keys {
                Value::Object(k) => sec.retain(|name, _| {
                    !k.contains_key(name) && !(name == "save_hotkey" && k.contains_key("save_hotkeys"))
                }),
                _ => sec.clear(),
            }
        }
        self.0.retain(|_, v| v.as_object().is_none_or(|m| !m.is_empty()));
    }
}

/// Pin this process to `mask` fitted to the CPUs that exist; returns its hex text on success.
pub(crate) fn apply_affinity(mask: usize) -> Option<String> {
    let system = system_cpu_mask();
    let Some(mask) = crate::record_config::fit_cpu_mask(mask, system) else {
        log!("record: {} CPUs, running unpinned, priority BelowNormal", system.count_ones());
        return None;
    };
    use windows::Win32::System::Threading::{GetCurrentProcess, SetProcessAffinityMask};
    // Retried once; a second refusal leaves the recorder unpinned at BelowNormal (logged).
    let pin = || unsafe { SetProcessAffinityMask(GetCurrentProcess(), mask) };
    match pin().or_else(|_| pin()) {
        Ok(()) => {
            log!("record: affinity=0x{mask:x}, priority BelowNormal");
            Some(format!("{mask:x}"))
        }
        Err(e) => {
            log!("record: affinity 0x{mask:x} failed ({e}); running unpinned");
            None
        }
    }
}

/// The machine's CPUs as an affinity mask (0 if Windows will not say).
pub(crate) fn system_cpu_mask() -> usize {
    use windows::Win32::System::Threading::{GetCurrentProcess, GetProcessAffinityMask};
    let (mut process, mut system) = (0usize, 0usize);
    let _ = unsafe { GetProcessAffinityMask(GetCurrentProcess(), &mut process, &mut system) };
    system
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::collections::HashMap;

    #[test]
    fn env_overrides_apply_and_are_forgotten_when_patched() {
        let vars: HashMap<&str, &str> = [
            ("RECALL_OUT_DIR", r"D:\Clips"),
            ("RECALL_BUFFER_SECS", "60"),
            ("RECALL_SAVE_HOTKEY", "Alt+N"),
            ("RECALL_OVERLAY", "1"),
            ("RECALL_CPU_MASK", "0xffff0000"),
        ]
        .into_iter()
        .collect();
        let mut env = EnvOverrides::from_lookup(|k| vars.get(k).map(|v| v.to_string()));
        let file = Config::default();
        let eff = env.apply(&file);
        assert_eq!(eff.recording.output_dir, r"D:\Clips");
        assert_eq!(eff.recording.buffer_secs, 60);
        assert!(eff.overlay.enabled);
        assert_eq!(file.recording.buffer_secs, 120, "the file config is untouched");

        // A set_config that touches buffer_secs drops only that override.
        env.forget(&json!({"recording":{"buffer_secs":90}}));
        let eff = env.apply(&file);
        assert_eq!(eff.recording.buffer_secs, 120, "file value wins once patched");
        assert_eq!(eff.recording.output_dir, r"D:\Clips", "other overrides remain");
        env.forget(&json!({"recording":{"output_dir":"x","save_hotkey":"x","cpu_mask":"x"},"overlay":{"enabled":false}}));
        assert!(env.is_empty(), "emptied sections are pruned");
    }

    #[test]
    fn env_save_hotkey_moves_the_full_buffer_preset_until_presets_are_patched() {
        let mut env = EnvOverrides::from_lookup(|k| (k == "RECALL_SAVE_HOTKEY").then(|| "Ctrl+F9".into()));
        let file = Config::default();
        let eff = env.apply(&file);
        assert_eq!(eff.recording.save_hotkeys[0].hotkey, "Ctrl+F9");
        assert_eq!(eff.recording.save_hotkey, "Ctrl+F9");
        env.forget(&json!({"recording":{"save_hotkeys":[{"hotkey":"Alt+N","seconds":0}]}}));
        assert!(env.is_empty(), "a presets patch retires the legacy override");
    }

    #[test]
    fn invalid_env_overrides_fall_back_to_file_config() {
        let env = EnvOverrides::from_lookup(|k| (k == "RECALL_OVERLAY_CORNER").then(|| "sideways".into()));
        let file = Config::default();
        assert_eq!(env.apply(&file), file);
        let junk = EnvOverrides::from_lookup(|k| (k == "RECALL_BUFFER_SECS").then(|| "lots".into()));
        assert!(junk.is_empty(), "a non-numeric buffer override is ignored");
    }
}
