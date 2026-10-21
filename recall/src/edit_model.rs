//! Nondestructive single-clip project and prepared-media protocol.
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TimeRange { pub start: f64, pub end: f64 }

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AudioSetting { pub stream_index: usize, pub gain_db: f64, pub muted: bool }

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
            previous_end = range.end;
        }
        let mut seen = std::collections::HashSet::new();
        for track in &self.audio {
            if !audio_indices.contains(&track.stream_index) || !seen.insert(track.stream_index) { return Err("audio track selection is invalid or duplicated".into()); }
            if !track.gain_db.is_finite() || !(-60.0..=12.0).contains(&track.gain_db) { return Err("audio gain must be between -60 and +12 dB".into()); }
        }
        Ok(())
    }
    pub fn duration(&self) -> f64 { self.ranges.iter().map(|r| r.end-r.start).sum() }
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
}
