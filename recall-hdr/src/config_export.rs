//! `[export]` -- the after-recording defaults: what the editor starts a clip's export at when the
//! clip has no saved edit. Rohan 2026-11-11: post-recording settings can never exceed the
//! recording settings, "because there is nothing extra in the recording to recover". The values
//! here are what the person chose; the editor caps each one at the current recording setting and
//! at what the clip was actually recorded at, and the Settings page greys out what is capped.
//! A capped value is kept, not lowered, so raising the recording setting brings it back.
//!
//! A file without `[export]` (0.1.0 to 0.1.3) loads with the defaults: 60 fps (Rohan 2026-11-11,
//! "Start at 60", as the editor started before; a clip recorded lower exports at its own rate),
//! the clip's own resolution, re-encode, no size limit.

use serde::{Deserialize, Serialize};

/// Export methods (`edit_model::ExportMode` in snake case).
pub const EXPORT_MODES: [&str; 2] = ["reencode", "lossless"];
/// Smallest export height (`edit_model::MIN_EXPORT_HEIGHT`).
pub const EXPORT_HEIGHT_MIN: u32 = 144;
/// Largest export height (8K); anything taller is a typo.
pub const EXPORT_HEIGHT_MAX: u32 = 4320;
/// Largest size limit in MB (100 GB); anything larger is a typo.
pub const EXPORT_MAX_MB_MAX: u32 = 100_000;
/// The export rate a clip with no saved edit starts at (never above the clip's own rate).
pub const EXPORT_FPS_DEFAULT: u32 = 60;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ExportCfg {
    /// Export frame rate. 0 = the clip's recorded rate.
    pub fps: u32,
    /// Output height in pixels. 0 = the clip's own resolution.
    pub height: u32,
    /// `reencode` | `lossless` (lossless trim keeps the recording's rate, size and bitrate).
    pub mode: String,
    /// File size limit in MB (re-encode only). 0 = no limit.
    pub max_mb: u32,
}

impl Default for ExportCfg {
    fn default() -> Self {
        Self { fps: EXPORT_FPS_DEFAULT, height: 0, mode: "reencode".into(), max_mb: 0 }
    }
}

/// The `set_config` guard for `[export]`.
pub fn validate(cfg: &ExportCfg) -> Result<(), String> {
    let fps_max = crate::record_config::FPS_MAX;
    if cfg.fps != 0 && !(1..=fps_max).contains(&cfg.fps) {
        return Err(format!("export.fps must be 0 (match the recording) or 1..={fps_max}, got {}", cfg.fps));
    }
    if cfg.height != 0 && !(EXPORT_HEIGHT_MIN..=EXPORT_HEIGHT_MAX).contains(&cfg.height) {
        return Err(format!("export.height must be 0 (the clip's own) or {EXPORT_HEIGHT_MIN}..={EXPORT_HEIGHT_MAX}, got {}", cfg.height));
    }
    if !EXPORT_MODES.contains(&cfg.mode.as_str()) {
        return Err(format!("export.mode '{}' must be one of {}", cfg.mode, EXPORT_MODES.join(" | ")));
    }
    if cfg.max_mb > EXPORT_MAX_MB_MAX {
        return Err(format!("export.max_mb must be 0 (no limit) or at most {EXPORT_MAX_MB_MAX}, got {}", cfg.max_mb));
    }
    Ok(())
}

/// Load-time repair (`config_validate`): each bad value takes its default, one note each.
pub fn repair(cfg: &mut ExportCfg, notes: &mut Vec<String>) {
    let fps_max = crate::record_config::FPS_MAX;
    if cfg.fps > fps_max {
        notes.push(format!("export.fps {} -> {fps_max} (allowed 0..={fps_max})", cfg.fps));
        cfg.fps = fps_max;
    }
    if cfg.height != 0 && cfg.height < EXPORT_HEIGHT_MIN {
        notes.push(format!("export.height {} -> {EXPORT_HEIGHT_MIN}", cfg.height));
        cfg.height = EXPORT_HEIGHT_MIN;
    } else if cfg.height > EXPORT_HEIGHT_MAX {
        notes.push(format!("export.height {} -> 0 (the clip's own)", cfg.height));
        cfg.height = 0;
    }
    if !EXPORT_MODES.contains(&cfg.mode.as_str()) {
        let folded = cfg.mode.trim().to_ascii_lowercase().replace(['-', '_', ' '], "");
        let fixed = EXPORT_MODES.iter().find(|m| **m == folded).copied().unwrap_or("reencode");
        notes.push(format!("export.mode '{}' -> '{fixed}'", cfg.mode));
        cfg.mode = fixed.into();
    }
    if cfg.max_mb > EXPORT_MAX_MB_MAX {
        notes.push(format!("export.max_mb {} -> 0 (no limit)", cfg.max_mb));
        cfg.max_mb = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_start_at_60_and_pass() {
        let cfg = ExportCfg::default();
        assert_eq!((cfg.fps, cfg.height, cfg.mode.as_str(), cfg.max_mb), (60, 0, "reencode", 0));
        validate(&cfg).unwrap();
    }

    #[test]
    fn bad_values_are_refused_by_a_patch_and_repaired_at_load() {
        for bad in [
            ExportCfg { fps: 999, ..Default::default() },
            ExportCfg { height: 100, ..Default::default() },
            ExportCfg { height: 99_999, ..Default::default() },
            ExportCfg { mode: "copy".into(), ..Default::default() },
            ExportCfg { max_mb: u32::MAX, ..Default::default() },
        ] {
            assert!(validate(&bad).is_err(), "{bad:?}");
            let mut fixed = bad.clone();
            let mut notes = Vec::new();
            repair(&mut fixed, &mut notes);
            assert_eq!(notes.len(), 1, "{bad:?}: one note per repair");
            validate(&fixed).unwrap();
        }
    }

    #[test]
    fn a_re_cased_mode_is_kept() {
        let mut cfg = ExportCfg { mode: "Lossless".into(), ..Default::default() };
        repair(&mut cfg, &mut Vec::new());
        assert_eq!(cfg.mode, "lossless");
    }

    /// Every released settings file (`compat/settings/v*`): one with no `[export]` (0.1.0 to 0.1.3) loads
    /// with the defaults (60 fps, as the editor started before), the load repairs nothing there,
    /// and a save writes those same defaults, so an existing install exports as before; one
    /// that has `[export]` (0.1.4 on) keeps every value, repaired nothing.
    #[test]
    fn released_settings_files_load_with_export_defaults() {
        let settings = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../compat/settings");
        let mut seen = 0;
        for dir in std::fs::read_dir(&settings).unwrap().flatten().map(|e| e.path()).filter(|p| p.join("config.toml").is_file()) {
            let bytes = std::fs::read(dir.join("config.toml")).unwrap();
            // 0.1.0 to 0.1.3 wrote no [export]: the defaults. A later release's file keeps its own values.
            let written: Option<ExportCfg> = toml::from_str::<toml::Value>(&String::from_utf8_lossy(&bytes))
                .unwrap()
                .get("export")
                .map(|t| t.clone().try_into().expect("a release's [export] parses"));
            let scratch = std::env::temp_dir().join(format!("recall-export-compat-{}-{seen}", std::process::id()));
            std::fs::create_dir_all(&scratch).unwrap();
            let path = scratch.join("config.toml");
            std::fs::write(&path, &bytes).unwrap();
            let cfg = crate::config::load_bytes(&path, &bytes, std::time::SystemTime::now());
            let repairs = crate::config_repairs::take(&path);
            assert!(repairs.iter().all(|r| !r.starts_with("export.")), "{}: {repairs:?}", dir.display());
            let expected = written.clone().unwrap_or_default();
            assert_eq!(cfg.export, expected, "{}", dir.display());
            cfg.save_to(&path).unwrap();
            let saved: toml::Value = toml::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
            let export: ExportCfg = saved.get("export").expect("the save writes [export]").clone().try_into().unwrap();
            assert_eq!(export, expected, "{}: the save keeps [export]", dir.display());
            let _ = std::fs::remove_dir_all(&scratch);
            seen += 1;
        }
        assert!(seen >= 3, "found {seen} released settings files");
    }
}
