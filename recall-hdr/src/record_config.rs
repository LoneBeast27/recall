//! Unified record-mode config: the `[recording]` / `[overlay]` sections, the
//! control-pipe `set_config` patch (deep-merge + schema check + validation), and
//! an injectable on-disk store so tests never touch `%APPDATA%`. The startup
//! environment overrides live in `record_env`.
//!
//! The controller is the single config writer. A patch has the same shape as the
//! TOML sections (`{"recording":{"buffer_secs":90}}`); objects merge recursively,
//! anything else (including the `overrides` array) replaces. Unknown keys are
//! rejected so a typo never silently does nothing.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::config::Config;

/// Valid `overlay.corner` tokens (the `recall-overlay --corner` vocabulary).
pub const CORNERS: [&str; 4] = ["top-left", "top-right", "bottom-left", "bottom-right"];
/// Accepted replay-buffer lengths (seconds). The ring lives in RAM.
pub const BUFFER_SECS_MIN: u32 = 10;
pub const BUFFER_SECS_MAX: u32 = 600;

/// `[recording]` -- what the unified record mode captures and how.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct RecordingCfg {
    /// Detect the foreground fullscreen game and record it automatically.
    pub auto_record: bool,
    /// Folder the engine writes clips into (`RECALL_OUT_DIR`).
    pub output_dir: String,
    /// Replay-buffer length in seconds; also the hotkey's save length.
    pub buffer_secs: u32,
    /// Global accelerator that saves the replay buffer.
    pub save_hotkey: String,
    /// Global accelerator that toggles the recorded microphone.
    pub mic_hotkey: String,
    /// Hex CPU affinity mask applied to the controller and every child.
    pub cpu_mask: String,
    /// With no game target, capture the primary monitor as "Desktop".
    pub record_desktop_when_idle: bool,
}

impl Default for RecordingCfg {
    fn default() -> Self {
        RecordingCfg {
            auto_record: true,
            output_dir: r"V:\ALL RECORDINGS".into(),
            buffer_secs: 120,
            save_hotkey: "Alt+N".into(),
            mic_hotkey: "Alt+M".into(),
            cpu_mask: "ffff0000".into(),
            record_desktop_when_idle: false,
        }
    }
}

/// `[overlay]` -- the controller-owned `recall-overlay.exe` toast process.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct OverlayCfg {
    pub enabled: bool,
    pub corner: String,
}

impl Default for OverlayCfg {
    fn default() -> Self {
        OverlayCfg { enabled: false, corner: "top-right".into() }
    }
}

/// Parse a hex affinity mask (`"ffff0000"` / `"0xffff0000"`); zero is invalid.
pub fn parse_cpu_mask(text: &str) -> Result<usize, String> {
    let raw = text.trim();
    let digits = raw.strip_prefix("0x").or_else(|| raw.strip_prefix("0X")).unwrap_or(raw);
    match usize::from_str_radix(digits, 16) {
        Ok(0) => Err("recording.cpu_mask must select at least one CPU".into()),
        Ok(mask) => Ok(mask),
        Err(_) => Err(format!("recording.cpu_mask '{text}' is not a hex mask")),
    }
}

/// Validate the record-mode values of a (merged) config.
pub fn validate(cfg: &Config) -> Result<(), String> {
    let rec = &cfg.recording;
    if !(BUFFER_SECS_MIN..=BUFFER_SECS_MAX).contains(&rec.buffer_secs) {
        return Err(format!(
            "recording.buffer_secs must be {BUFFER_SECS_MIN}..={BUFFER_SECS_MAX}, got {}",
            rec.buffer_secs
        ));
    }
    if rec.output_dir.trim().is_empty() {
        return Err("recording.output_dir must not be empty".into());
    }
    parse_cpu_mask(&rec.cpu_mask)?;
    crate::hotkey::parse_accel(&rec.save_hotkey).map_err(|e| format!("recording.save_hotkey: {e}"))?;
    crate::hotkey::parse_accel(&rec.mic_hotkey).map_err(|e| format!("recording.mic_hotkey: {e}"))?;
    if rec.save_hotkey.trim().eq_ignore_ascii_case(rec.mic_hotkey.trim()) {
        return Err("recording.save_hotkey and recording.mic_hotkey must differ".into());
    }
    if !CORNERS.contains(&cfg.overlay.corner.as_str()) {
        return Err(format!(
            "overlay.corner '{}' must be one of {}",
            cfg.overlay.corner,
            CORNERS.join(" | ")
        ));
    }
    Ok(())
}

/// Deep-merge `patch` into `base`: objects merge key by key, every other value
/// (scalars, arrays, null) replaces.
pub fn merge_json(base: &mut Value, patch: &Value) {
    match (base, patch) {
        (Value::Object(b), Value::Object(p)) => {
            for (k, v) in p {
                match b.get_mut(k) {
                    Some(slot) if slot.is_object() && v.is_object() => merge_json(slot, v),
                    _ => {
                        b.insert(k.clone(), v.clone());
                    }
                }
            }
        }
        (slot, v) => *slot = v.clone(),
    }
}

/// Optional schema keys that are absent from the serialized form when unset.
const OPTIONAL_KEYS: &[&str] = &["display.target_device_path"];

/// Reject patch keys the schema does not have (typo guard) and `version`.
fn check_keys(schema: &Map<String, Value>, patch: &Map<String, Value>, prefix: &str) -> Result<(), String> {
    for (k, v) in patch {
        let path = if prefix.is_empty() { k.clone() } else { format!("{prefix}.{k}") };
        if path == "version" {
            return Err("config key 'version' is read-only".into());
        }
        match schema.get(k) {
            Some(Value::Object(inner)) => match v {
                Value::Object(p) => check_keys(inner, p, &path)?,
                _ => return Err(format!("config key '{path}' must be an object")),
            },
            Some(_) => {}
            None if OPTIONAL_KEYS.contains(&path.as_str()) => {}
            None => return Err(format!("unknown config key '{path}'")),
        }
    }
    Ok(())
}

/// Collect dotted leaf paths whose values differ between `a` and `b`.
fn diff_leaves(a: &Value, b: &Value, prefix: &str, out: &mut Vec<String>) {
    match (a, b) {
        (Value::Object(x), Value::Object(y)) => {
            let mut keys: Vec<&String> = x.keys().chain(y.keys()).collect();
            keys.sort();
            keys.dedup();
            for k in keys {
                let path = if prefix.is_empty() { k.clone() } else { format!("{prefix}.{k}") };
                diff_leaves(x.get(k).unwrap_or(&Value::Null), y.get(k).unwrap_or(&Value::Null), &path, out);
            }
        }
        _ if a != b => out.push(prefix.to_string()),
        _ => {}
    }
}

/// The result of a successful `set_config` patch.
#[derive(Debug, Clone, PartialEq)]
pub struct PatchOutcome {
    pub config: Config,
    /// Dotted leaf keys whose value actually changed (e.g. `overlay.enabled`).
    pub changed: Vec<String>,
}

impl PatchOutcome {
    /// Did any key under `prefix` (a section like `"overlay"` or a leaf) change?
    pub fn touched(&self, prefix: &str) -> bool {
        self.changed
            .iter()
            .any(|k| k == prefix || k.starts_with(&format!("{prefix}.")))
    }
}

/// Apply a `set_config` patch to `base`: schema check, merge, parse, validate.
/// Nothing is applied on error.
pub fn apply_patch(base: &Config, patch: &Value) -> Result<PatchOutcome, String> {
    let Value::Object(patch_map) = patch else {
        return Err("set_config.patch must be an object".into());
    };
    let before = serde_json::to_value(base).map_err(|e| format!("serialize config: {e}"))?;
    let Value::Object(schema) = &before else {
        return Err("config did not serialize to an object".into());
    };
    check_keys(schema, patch_map, "")?;
    let mut merged = before.clone();
    merge_json(&mut merged, patch);
    let config: Config = serde_json::from_value(merged).map_err(|e| format!("invalid config: {e}"))?;
    validate(&config)?;
    let after = serde_json::to_value(&config).map_err(|e| format!("serialize config: {e}"))?;
    let mut changed = Vec::new();
    diff_leaves(&before, &after, "", &mut changed);
    Ok(PatchOutcome { config, changed })
}

/// The full config as the pipe's `config` event payload.
pub fn config_json(cfg: &Config) -> Value {
    serde_json::to_value(cfg).unwrap_or(Value::Null)
}

/// Where the unified mode loads/saves its config. Tests point it at a temp dir.
#[derive(Debug, Clone)]
pub struct ConfigStore {
    path: PathBuf,
}

impl ConfigStore {
    /// `%APPDATA%\recall-hdr\config.toml` -- the user's real config.
    pub fn user() -> Self {
        ConfigStore { path: Config::path() }
    }

    pub fn at(path: impl Into<PathBuf>) -> Self {
        ConfigStore { path: path.into() }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn load(&self) -> Config {
        crate::config::load_from(&self.path)
    }

    pub fn save(&self, cfg: &Config) -> Result<(), String> {
        cfg.save_to(&self.path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::RuleKind;
    use serde_json::json;

    /// The user's real config shape (2026-10-22): autostart off, 360-nit SDR
    /// white, a `brave -> none` override, comments, and no `[recording]` yet.
    const USER_TOML: &str = r#"
version = 1
[display]
target_friendly_name = "PG27AQDP"
[behavior]
default_game_class = "sdr_game"
default_clamp = "punchy"
# SDR-content white level comment
sdr_white_nits = 360
debounce_ms = 400
[hotkey]
toggle_hdr = "Ctrl+Alt+H"
[startup]
autostart = false
[[overrides]]
exe = "hdr-testcard"
kind = "hdr_game"
[[overrides]]
exe = "brave"
kind = "none"
"#;

    fn temp_store(tag: &str) -> ConfigStore {
        let dir = std::env::temp_dir().join(format!(
            "recall-hdr-reccfg-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        ConfigStore::at(dir.join("config.toml"))
    }

    fn user_cfg() -> Config {
        toml::from_str(USER_TOML).expect("user config parses")
    }

    #[test]
    fn user_config_without_recording_sections_gets_defaults() {
        let cfg = user_cfg();
        assert_eq!(cfg.recording, RecordingCfg::default());
        assert_eq!(cfg.overlay, OverlayCfg::default());
        assert!(!cfg.behavior.hdr_auto, "hdr_auto defaults off");
        assert_eq!(cfg.recording.output_dir, r"V:\ALL RECORDINGS");
        assert_eq!(cfg.recording.buffer_secs, 120);
        assert_eq!(cfg.recording.cpu_mask, "ffff0000");
    }

    #[test]
    fn partial_recording_section_fills_missing_keys() {
        let src = format!("{USER_TOML}\n[recording]\nbuffer_secs = 90\n");
        // `[recording]` must precede `[[overrides]]` only for serializing; TOML
        // allows it anywhere on load.
        let cfg: Config = toml::from_str(&src).unwrap();
        assert_eq!(cfg.recording.buffer_secs, 90);
        assert_eq!(cfg.recording.save_hotkey, "Alt+N");
        assert!(cfg.recording.auto_record);
    }

    #[test]
    fn load_patch_save_reload_preserves_user_values() {
        let store = temp_store("roundtrip");
        std::fs::write(store.path(), USER_TOML).unwrap();
        let loaded = store.load();
        let out = apply_patch(
            &loaded,
            &json!({"overlay":{"enabled":true,"corner":"bottom-left"},"recording":{"buffer_secs":90}}),
        )
        .unwrap();
        store.save(&out.config).unwrap();
        let back = store.load();
        // The patched values landed...
        assert!(back.overlay.enabled);
        assert_eq!(back.overlay.corner, "bottom-left");
        assert_eq!(back.recording.buffer_secs, 90);
        // ...and every user value survived the round trip.
        assert!(!back.startup.autostart);
        assert_eq!(back.behavior.sdr_white_nits, 360);
        assert_eq!(back.overrides.len(), 2);
        assert_eq!(back.overrides[1].exe, "brave");
        assert_eq!(back.overrides[1].kind, RuleKind::None);
        assert_eq!(back, out.config, "save -> load is identity");
        let _ = std::fs::remove_dir_all(store.path().parent().unwrap());
    }

    #[test]
    fn patch_reports_only_changed_leaves() {
        let base = user_cfg();
        let out = apply_patch(
            &base,
            &json!({"startup":{"autostart":true},"overlay":{"corner":"top-right"}}),
        )
        .unwrap();
        assert_eq!(out.changed, vec!["startup.autostart".to_string()], "corner was already top-right");
        assert!(out.touched("startup"));
        assert!(!out.touched("overlay"));
        assert!(!out.touched("start"), "prefix match is per path segment");
    }

    #[test]
    fn patch_rejects_unknown_keys_bad_values_and_version() {
        let base = user_cfg();
        for (patch, needle) in [
            (json!({"recording":{"bufer_secs":90}}), "unknown config key 'recording.bufer_secs'"),
            (json!({"nope":1}), "unknown config key 'nope'"),
            (json!({"recording":5}), "must be an object"),
            (json!({"overlay":{"corner":"middle"}}), "overlay.corner"),
            (json!({"recording":{"buffer_secs":5}}), "buffer_secs"),
            (json!({"recording":{"cpu_mask":"0"}}), "cpu_mask"),
            (json!({"recording":{"cpu_mask":"zz"}}), "cpu_mask"),
            (json!({"recording":{"save_hotkey":"Ctrl+Nonsense"}}), "save_hotkey"),
            (json!({"recording":{"save_hotkey":"Alt+M"}}), "must differ"),
            (json!({"recording":{"buffer_secs":"ninety"}}), "invalid config"),
            (json!({"version":2}), "read-only"),
            (json!([1, 2]), "must be an object"),
        ] {
            let err = apply_patch(&base, &patch).expect_err(&format!("{patch} must be rejected"));
            assert!(err.contains(needle), "{patch}: '{err}' lacks '{needle}'");
        }
    }

    #[test]
    fn patch_replaces_overrides_array_and_allows_optional_device_path() {
        let base = user_cfg();
        let out = apply_patch(
            &base,
            &json!({"overrides":[{"exe":"valorant","kind":"sdr_game"}],"display":{"target_device_path":"\\\\?\\DISPLAY#X"}}),
        )
        .unwrap();
        assert_eq!(out.config.overrides.len(), 1);
        assert_eq!(out.config.overrides[0].exe, "valorant");
        assert!(out.config.display.target_device_path.is_some());
        assert!(out.touched("overrides"));
    }

    #[test]
    fn merge_json_merges_objects_and_replaces_everything_else() {
        let mut base = json!({"a":{"x":1,"y":2},"list":[1,2],"s":"old"});
        merge_json(&mut base, &json!({"a":{"y":3},"list":[9],"s":"new"}));
        assert_eq!(base, json!({"a":{"x":1,"y":3},"list":[9],"s":"new"}));
    }

    #[test]
    fn cpu_mask_parses_with_or_without_prefix() {
        assert_eq!(parse_cpu_mask("ffff0000"), Ok(0xffff_0000));
        assert_eq!(parse_cpu_mask("0xFFFF0000"), Ok(0xffff_0000));
        assert!(parse_cpu_mask("0").is_err());
        assert!(parse_cpu_mask("").is_err());
    }

    #[test]
    fn config_json_has_every_section() {
        let v = config_json(&user_cfg());
        for key in ["display", "behavior", "hotkey", "startup", "recording", "overlay", "overrides"] {
            assert!(v.get(key).is_some(), "config event lacks '{key}'");
        }
        assert_eq!(v["behavior"]["default_clamp"], "punchy", "snake_case tokens on the wire");
        assert_eq!(v["recording"]["save_hotkey"], "Alt+N");
    }
}
