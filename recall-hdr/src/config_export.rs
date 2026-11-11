//! `[export]` -- the after-recording defaults: what the editor starts a clip's export at when the
//! clip has no saved edit. Rohan 2026-11-11: post-recording settings can never exceed the
//! recording settings, "because there is nothing extra in the recording to recover". The values
//! here are what the person chose; the editor caps each one at the current recording setting and
//! at what the clip was actually recorded at, and the Settings page greys out what is capped.
//! A capped value is kept, not lowered, so raising the recording setting brings it back.
//!
//! A file without `[export]` (0.1.0 to 0.1.3) loads with every key at "match the recording".

use serde::{Deserialize, Serialize};

/// Export methods (`edit_model::ExportMode` in snake case).
pub const EXPORT_MODES: [&str; 2] = ["reencode", "lossless"];
/// Smallest export height (`edit_model::MIN_EXPORT_HEIGHT`).
pub const EXPORT_HEIGHT_MIN: u32 = 144;
/// Largest export height (8K); anything taller is a typo.
pub const EXPORT_HEIGHT_MAX: u32 = 4320;
/// Largest size limit in MB (100 GB); anything larger is a typo.
pub const EXPORT_MAX_MB_MAX: u32 = 100_000;

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
        Self { fps: 0, height: 0, mode: "reencode".into(), max_mb: 0 }
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
    fn defaults_match_the_recording_and_pass() {
        let cfg = ExportCfg::default();
        assert_eq!((cfg.fps, cfg.height, cfg.mode.as_str(), cfg.max_mb), (0, 0, "reencode", 0));
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
}
