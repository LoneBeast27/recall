//! The after-recording defaults (Settings → Recording → After recording; `[export]` in the
//! controller config) and their caps. Rohan 2026-11-11: a post-recording value can never exceed
//! the recording value, "because there is nothing extra in the recording to recover". So the
//! value a clip starts its export at is the lowest of the saved default, the current recording
//! setting and what the clip itself was recorded at. The saved default is never lowered: raising
//! the recording setting brings it back.
use serde_json::Value;

use crate::edit_model::{ExportMode, ExportSettings, MIN_EXPORT_HEIGHT};

/// The rate a clip with no saved edit starts at when the config names none (`[export].fps`'s
/// default; Rohan 2026-11-11 "Start at 60").
pub const FPS_DEFAULT: u32 = 60;

/// The `[export]` values, plus the recording frame rate that caps them.
#[derive(Clone, Debug, PartialEq)]
pub struct ExportDefaults {
    /// 0 = the clip's recorded rate.
    pub fps: u32,
    /// 0 = the clip's own resolution.
    pub height: u32,
    pub lossless: bool,
    /// 0 = no limit.
    pub max_mb: u32,
    /// `recording.fps`; 0 when unknown (no cap from it).
    pub recording_fps: u32,
}

impl Default for ExportDefaults {
    fn default() -> Self {
        Self { fps: FPS_DEFAULT, height: 0, lossless: false, max_mb: 0, recording_fps: 0 }
    }
}

impl ExportDefaults {
    /// From the controller's config JSON (`None` before it arrives, or a key missing: the defaults).
    pub fn from_config(config: Option<&Value>) -> Self {
        let read = |section: &str, key: &str| config.and_then(|c| c.get(section)?.get(key)?.as_u64()).map(|n| u32::try_from(n).unwrap_or(u32::MAX));
        let num = |section: &str, key: &str| read(section, key).unwrap_or(0);
        let mode = config.and_then(|c| c.get("export")?.get("mode")?.as_str()).unwrap_or("reencode");
        Self {
            fps: read("export", "fps").unwrap_or(FPS_DEFAULT),
            height: num("export", "height"),
            lossless: mode == "lossless",
            max_mb: num("export", "max_mb"),
            recording_fps: num("recording", "fps"),
        }
    }

    /// The default rate after the recording setting's cap (0 = match the clip).
    pub fn capped_fps(&self) -> u32 {
        match (self.fps, self.recording_fps) {
            (0, _) => 0,
            (fps, 0) => fps,
            (fps, rec) => fps.min(rec),
        }
    }

    /// Is the saved rate held down by the recording setting?
    pub fn fps_capped_by_recording(&self) -> bool {
        self.recording_fps != 0 && self.fps > self.recording_fps
    }

    /// The export rate a clip recorded at `recorded` fps starts at.
    pub fn fps_for(&self, recorded: u32) -> u32 {
        match self.capped_fps() {
            0 => recorded,
            fps => fps.min(recorded),
        }
        .max(1)
    }

    /// The export options a clip `clip_height` pixels tall starts with.
    pub fn settings_for(&self, clip_height: u32) -> ExportSettings {
        if self.lossless {
            // Lossless trim keeps the recording's rate, size and bitrate: nothing to cap.
            return ExportSettings { mode: ExportMode::Lossless, ..ExportSettings::default() };
        }
        let height = (self.height >= MIN_EXPORT_HEIGHT && (clip_height == 0 || self.height < clip_height)).then_some(self.height);
        let max_bytes = (self.max_mb > 0).then(|| u64::from(self.max_mb) * 1_000_000);
        ExportSettings { mode: ExportMode::Reencode, height, max_bytes, ..ExportSettings::default() }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn defaults(export: Value, rec_fps: u64) -> ExportDefaults {
        ExportDefaults::from_config(Some(&json!({ "export": export, "recording": { "fps": rec_fps } })))
    }

    #[test]
    fn an_old_config_without_export_starts_at_60() {
        let d = ExportDefaults::from_config(Some(&json!({ "recording": { "fps": 144 } })));
        assert_eq!((d.fps, d.height, d.lossless, d.max_mb, d.recording_fps), (60, 0, false, 0, 144));
        assert_eq!(d.fps_for(144), 60, "Rohan 2026-11-11: start at 60");
        assert_eq!(d.fps_for(30), 30, "a clip recorded lower keeps its own rate");
        assert_eq!(d.settings_for(1440), ExportSettings::default());
        assert_eq!(ExportDefaults::from_config(None).fps_for(144), 60, "no config yet");
        let matched = defaults(json!({ "fps": 0 }), 60);
        assert_eq!(matched.fps_for(144), 144, "Match = the clip's own rate, even above today's setting");
    }

    #[test]
    fn the_lowest_of_default_recording_and_clip_wins() {
        let d = defaults(json!({ "fps": 120 }), 60);
        assert_eq!(d.capped_fps(), 60);
        assert!(d.fps_capped_by_recording());
        assert_eq!(d.fps_for(30), 30, "a 30 fps clip");
        assert_eq!(d.fps_for(120), 60, "the recording setting caps the default");
        let raised = defaults(json!({ "fps": 120 }), 144);
        assert_eq!(raised.fps_for(144), 120, "raising the recording brings the saved default back");
        assert!(!raised.fps_capped_by_recording());
    }

    #[test]
    fn resolution_and_size_apply_only_below_the_clip_and_only_re_encoded() {
        let d = defaults(json!({ "height": 1080, "max_mb": 50 }), 60);
        let s = d.settings_for(1440);
        assert_eq!((s.mode, s.height, s.max_bytes), (ExportMode::Reencode, Some(1080), Some(50_000_000)));
        assert_eq!(d.settings_for(1080).height, None, "a 1080p clip keeps its own size");
        assert_eq!(d.settings_for(720).height, None, "never above the clip");
        let lossless = defaults(json!({ "mode": "lossless", "height": 720, "max_mb": 10 }), 60).settings_for(1440);
        assert_eq!(lossless, ExportSettings { mode: ExportMode::Lossless, ..ExportSettings::default() });
        lossless.validate(&Default::default()).unwrap();
    }
}
