//! The pipe's `config` event: the effective config plus what the app needs around it.
//! - `overridden`: dotted keys a startup env override sets (`Start-Recall.ps1 -OutDir`). The
//!   app does not repoint the library (folders.json) to an overridden `recording.output_dir`.
//! - `hotkey_warnings`: shortcuts set to a combo games and overlays often use
//!   (`record_hotkeys::GAMER_COMBOS`). A warning only: the shortcut stays bound.

use serde_json::json;

use crate::config::Config;
use crate::record_env::EnvOverrides;

pub fn config_event(cfg: &Config, overrides: &EnvOverrides) -> String {
    json!({
        "type": "config",
        "config": crate::record_config::config_json(cfg),
        "overridden": overrides.keys(),
        "hotkey_warnings": crate::record_hotkeys::clash_warnings(&cfg.recording),
    })
    .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;

    #[test]
    fn the_event_names_env_overridden_keys() {
        let env = EnvOverrides::from_lookup(|k| (k == "RECALL_OUT_DIR").then(|| r"D:\Tmp".to_string()));
        let cfg = env.apply(&Config::default());
        let ev: Value = serde_json::from_str(&config_event(&cfg, &env)).unwrap();
        assert_eq!(ev["config"]["recording"]["output_dir"], r"D:\Tmp");
        assert_eq!(ev["overridden"], serde_json::json!(["recording.output_dir"]));
        let plain: Value = serde_json::from_str(&config_event(&Config::default(), &EnvOverrides::default())).unwrap();
        assert_eq!(plain["overridden"], serde_json::json!([]));
    }

    #[test]
    fn a_shortcut_games_often_use_comes_with_a_warning() {
        let mut cfg = Config::default();
        cfg.recording.bookmark_hotkey = "Alt+Z".into();
        let ev: Value = serde_json::from_str(&config_event(&cfg, &EnvOverrides::default())).unwrap();
        let warnings = ev["hotkey_warnings"].as_array().cloned().unwrap_or_default();
        assert_eq!(warnings.len(), 1, "{ev}");
        assert!(warnings[0].as_str().unwrap().contains("Alt+Z"));
        let plain: Value = serde_json::from_str(&config_event(&Config::default(), &EnvOverrides::default())).unwrap();
        assert_eq!(plain["hotkey_warnings"], serde_json::json!([]), "the defaults clash with nothing");
    }
}
