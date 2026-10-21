//! Nondestructive single-clip project and prepared-media protocol.
//!
//! Compiled into both `recall` and `recall-native` (via `#[path]`), so it stays
//! dependency-free beyond serde, and every field added after v1 carries a serde
//! default so older project files and caches keep loading.
use serde::{Deserialize, Serialize};

/// Slowest and fastest per-range playback speed an export accepts.
pub const MIN_SPEED: f64 = 0.25;
pub const MAX_SPEED: f64 = 4.0;

fn unit_speed() -> f64 { 1.0 }

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TimeRange {
    pub start: f64,
    pub end: f64,
    /// Playback speed of this range in the export (0.25–4.0, 1.0 = unchanged).
    /// The output length of the range is `(end - start) / speed`.
    #[serde(default = "unit_speed")]
    pub speed: f64,
}

impl TimeRange {
    /// A range at normal speed.
    pub fn new(start: f64, end: f64) -> Self { Self { start, end, speed: 1.0 } }
    /// Seconds this range occupies in the exported file.
    #[allow(dead_code)] // the editor UI (recall-native) reads it; the engine uses the µs timeline
    pub fn output_len(&self) -> f64 { (self.end - self.start) / self.speed }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AudioSetting { pub stream_index: usize, pub gain_db: f64, pub muted: bool }

impl AudioSetting {
    /// True when the track leaves the export bit-for-bit (no gain, not muted).
    pub fn is_neutral(&self) -> bool { !self.muted && self.gain_db == 0.0 }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct EditPlan {
    pub ranges: Vec<TimeRange>,
    #[serde(default)]
    pub audio: Vec<AudioSetting>,
}

impl EditPlan {
    pub fn validate(&self, source_duration: f64, audio_indices: &[usize]) -> Result<(), String> {
        if !source_duration.is_finite() || source_duration <= 0.0 { return Err("source duration is invalid".into()); }
        if self.ranges.is_empty() || self.ranges.len() > 1000 { return Err("keep between 1 and 1000 clip ranges".into()); }
        let mut previous_end = 0.0;
        for (index, range) in self.ranges.iter().enumerate() {
            if !range.start.is_finite() || !range.end.is_finite() || range.start < 0.0 || range.end <= range.start || range.end > source_duration + 0.000_01 {
                return Err(format!("range {} is outside the source clip", index + 1));
            }
            if index > 0 && range.start < previous_end - 0.000_001 { return Err("clip ranges must be ordered and non-overlapping".into()); }
            if !range.speed.is_finite() || !(MIN_SPEED..=MAX_SPEED).contains(&range.speed) {
                return Err(format!("range {} speed must be between {MIN_SPEED}x and {MAX_SPEED}x", index + 1));
            }
            previous_end = range.end;
        }
        let mut seen = std::collections::HashSet::new();
        for track in &self.audio {
            if !audio_indices.contains(&track.stream_index) || !seen.insert(track.stream_index) { return Err("audio track selection is invalid or duplicated".into()); }
            if !track.gain_db.is_finite() || !(-60.0..=12.0).contains(&track.gain_db) { return Err("audio gain must be between -60 and +12 dB".into()); }
        }
        Ok(())
    }
    /// Length of the exported file in seconds: Σ (range length / speed).
    #[allow(dead_code)] // read by recall-native's editor
    pub fn duration(&self) -> f64 { self.ranges.iter().map(TimeRange::output_len).sum() }
    /// Seconds of source material kept, ignoring speed.
    #[allow(dead_code)] // read by recall-native's editor
    pub fn source_duration(&self) -> f64 { self.ranges.iter().map(|r| r.end - r.start).sum() }
    /// True when any range plays at a speed other than 1.0.
    pub fn has_speed_changes(&self) -> bool { self.ranges.iter().any(|r| r.speed != 1.0) }
}

/// How an edited export produces its video.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ExportMode {
    /// Decode and re-encode at the chosen CFR rate (supports speed, size cap, downscale).
    #[default]
    Reencode,
    /// Stream-copy the kept video; each cut starts at the keyframe at/before it.
    Lossless,
}

/// Per-export options carried in the project JSON as `export`.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct ExportSettings {
    #[serde(default)]
    pub mode: ExportMode,
    /// Upper bound for the finished file in bytes (re-encode only).
    #[serde(default)]
    pub max_bytes: Option<u64>,
    /// Output height in pixels (re-encode only); width keeps the aspect ratio and
    /// both are rounded to even numbers. A height at or above the source keeps it.
    #[serde(default)]
    pub height: Option<u32>,
}

/// Smallest output height an export accepts.
pub const MIN_EXPORT_HEIGHT: u32 = 144;

impl ExportSettings {
    pub fn validate(&self, plan: &EditPlan) -> Result<(), String> {
        if let Some(height) = self.height {
            if height < MIN_EXPORT_HEIGHT { return Err(format!("export height must be at least {MIN_EXPORT_HEIGHT} pixels")); }
        }
        if self.max_bytes == Some(0) { return Err("export size cap must be larger than zero".into()); }
        if self.mode == ExportMode::Lossless {
            if plan.has_speed_changes() { return Err("lossless trim keeps the original frames, so it cannot change speed; use a re-encoded export for slow motion".into()); }
            if self.max_bytes.is_some() { return Err("lossless trim keeps the original bitrate, so it cannot meet a size cap; use a re-encoded export".into()); }
            if self.height.is_some() { return Err("lossless trim keeps the original resolution; use a re-encoded export to downscale".into()); }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AudioTrackInfo {
    pub stream_index: usize, pub name: String, pub channels: u16,
    pub sample_rate: u32, pub preview_path: String, pub waveform: Vec<f32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClipInfo {
    pub path: String, pub duration: f64, pub width: u32, pub height: u32,
    pub video_fps: f64, pub source_bytes: u64, pub source_modified_ms: u64,
    pub audio: Vec<AudioTrackInfo>,
    /// Video keyframe times in seconds on the editor timeline (ascending). A
    /// lossless cut snaps its start to the last keyframe at or before it.
    #[serde(default)]
    pub keyframes: Vec<f64>,
}

#[allow(dead_code)] // used by recall-native's editor; the engine snaps inside the export
impl ClipInfo {
    /// The keyframe a lossless cut starting at `seconds` begins on.
    pub fn snap_to_keyframe(&self, seconds: f64) -> f64 { snap_to_keyframe(&self.keyframes, seconds) }
}

/// Last keyframe at or before `seconds` (1 µs tolerance); the first keyframe when
/// none precedes it; `seconds` itself when the list is empty.
pub fn snap_to_keyframe(keyframes: &[f64], seconds: f64) -> f64 {
    let at_or_before = keyframes.partition_point(|&k| k <= seconds + 0.000_001);
    match at_or_before {
        0 => keyframes.first().copied().unwrap_or(seconds),
        n => keyframes[n - 1],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_one_project_files_still_load() {
        let plan: EditPlan = serde_json::from_str(r#"{"ranges":[{"start":1.0,"end":3.5}],"audio":[{"stream_index":1,"gain_db":0.0,"muted":false}]}"#).unwrap();
        assert_eq!(plan.ranges[0], TimeRange::new(1.0, 3.5));
        let info: ClipInfo = serde_json::from_str(r#"{"path":"a.mp4","duration":2.0,"width":4,"height":2,"video_fps":60.0,"source_bytes":1,"source_modified_ms":1,"audio":[]}"#).unwrap();
        assert!(info.keyframes.is_empty());
        let settings: ExportSettings = serde_json::from_str("{}").unwrap();
        assert_eq!(settings, ExportSettings::default());
        assert_eq!(settings.mode, ExportMode::Reencode);
    }

    #[test]
    fn duration_is_output_length_after_speed() {
        let plan = EditPlan { ranges: vec![TimeRange { start: 0.0, end: 2.0, speed: 0.5 }, TimeRange { start: 3.0, end: 5.0, speed: 2.0 }], audio: vec![] };
        assert!((plan.duration() - 5.0).abs() < 1e-9);
        assert!((plan.source_duration() - 4.0).abs() < 1e-9);
        assert!(plan.validate(6.0, &[]).is_ok());
    }

    #[test]
    fn speed_and_lossless_combinations_are_validated() {
        let mut plan = EditPlan { ranges: vec![TimeRange { start: 0.0, end: 1.0, speed: 0.2 }], audio: vec![] };
        assert!(plan.validate(2.0, &[]).unwrap_err().contains("speed"));
        plan.ranges[0].speed = 0.5;
        let lossless = ExportSettings { mode: ExportMode::Lossless, ..Default::default() };
        assert!(lossless.validate(&plan).unwrap_err().contains("speed"));
        plan.ranges[0].speed = 1.0;
        assert!(lossless.validate(&plan).is_ok());
        let capped = ExportSettings { max_bytes: Some(1), ..lossless.clone() };
        assert!(capped.validate(&plan).unwrap_err().contains("size cap"));
        assert!(ExportSettings { height: Some(100), ..Default::default() }.validate(&plan).is_err());
    }

    #[test]
    fn snapping_picks_the_keyframe_at_or_before() {
        let keys = [0.0, 0.5, 1.0];
        assert_eq!(snap_to_keyframe(&keys, 0.7), 0.5);
        assert_eq!(snap_to_keyframe(&keys, 1.0), 1.0);
        assert_eq!(snap_to_keyframe(&keys, 9.0), 1.0);
        assert_eq!(snap_to_keyframe(&[0.2], 0.1), 0.2);
        assert_eq!(snap_to_keyframe(&[], 0.3), 0.3);
    }

    #[test]
    fn export_settings_round_trip_as_snake_case() {
        let settings = ExportSettings { mode: ExportMode::Lossless, max_bytes: None, height: None };
        let text = serde_json::to_string(&settings).unwrap();
        assert_eq!(text, r#"{"mode":"lossless","max_bytes":null,"height":null}"#);
        assert_eq!(serde_json::from_str::<ExportSettings>(&text).unwrap(), settings);
    }
}
