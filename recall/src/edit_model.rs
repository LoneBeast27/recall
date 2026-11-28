//! Nondestructive single-clip project and prepared-media protocol.
//!
//! Compiled into both `recall` and `recall-native` (via `#[path]`), so it stays
//! dependency-free beyond serde and serde_json, and every field added after v1 carries a serde
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

fn yes() -> bool { true }

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AudioSetting {
    pub stream_index: usize, pub gain_db: f64, pub muted: bool,
    /// "In mix": while merging is on, this track is summed into the one "Mix" track (default
    /// on, for every source); off, it follows the Mix as its own track. Ignored while merging is off.
    #[serde(default = "yes")]
    pub mix: bool,
}

/// One audio track an export writes, in output order (see `EditPlan::audio_outputs`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AudioOutput {
    /// Two or more source streams summed into one track named "Mix".
    Mix(Vec<usize>),
    /// One source stream on its own, keeping its name.
    Track(usize),
}

impl AudioSetting {
    /// True when the track leaves the export bit-for-bit (no gain, not muted).
    pub fn is_neutral(&self) -> bool { !self.muted && self.gain_db == 0.0 }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct EditPlan {
    pub ranges: Vec<TimeRange>,
    #[serde(default)]
    pub audio: Vec<AudioSetting>,
    /// This clip's "Merge audio" switch: the in-mix tracks become one "Mix" track, or every
    /// track is exported separately. `None` (never set for this clip): Settings > Audio >
    /// "Merge audio together on export" decides; the editor fills it in for each export, and
    /// the engine reads `None` as on.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mix_down: Option<bool>,
    /// How the merged "Mix" track is levelled: Settings > Audio > "Merge audio", not a per-clip
    /// edit. The editor fills it in for each export; the default (0 dB, limiter on) is what
    /// earlier builds did, so it is never written for it and older files load as it.
    #[serde(default, skip_serializing_if = "MixLevel::is_default")]
    pub mix_level: MixLevel,
}

/// Mix track level: a gain offset on the summed tracks, then the -1 dBFS look-ahead limiter
/// ("Prevent clipping"); without it the sum is only clamped to full scale.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
pub struct MixLevel {
    #[serde(default)]
    pub offset_db: f32,
    #[serde(default = "yes")]
    pub limiter: bool,
}

/// Bounds and step of the Mix level offset.
pub const MIX_OFFSET_MAX_DB: f32 = 12.0;
#[allow(dead_code)] // the Settings stepper (recall-native) reads it
pub const MIX_OFFSET_STEP_DB: f32 = 0.5;

impl Default for MixLevel {
    fn default() -> Self { Self { offset_db: 0.0, limiter: true } }
}

impl MixLevel {
    pub fn is_default(&self) -> bool { *self == Self::default() }
    /// The linear gain the summed Mix is multiplied by (1.0 for 0 dB).
    #[allow(dead_code)] // the engine applies it; the editor only passes the level on
    pub fn gain(&self) -> f32 { if self.offset_db == 0.0 { 1.0 } else { 10f32.powf(self.offset_db / 20.0) } }
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
        if !self.mix_level.offset_db.is_finite() || self.mix_level.offset_db.abs() > MIX_OFFSET_MAX_DB {
            return Err("mix level offset must be between -12 and +12 dB".into());
        }
        Ok(())
    }
    /// The audio tracks an export writes, first to last. Merging (`mix_down`, `None` = on): the
    /// kept tracks in the mix as one "Mix" track (a lone one stays itself), then the kept tracks
    /// switched out of the mix, each on its own, in stream order. Not merging: every kept track
    /// on its own, in stream order. Muted tracks are dropped. A stream with no setting is in the
    /// mix, unmuted, at 0 dB (`ClipInfo::default_setting`), as the editor shows it.
    pub fn audio_outputs(&self, audio_indices: &[usize]) -> Vec<AudioOutput> {
        let setting = |index: usize| self.audio.iter().find(|s| s.stream_index == index);
        let kept = audio_indices.iter().copied().filter(|i| !setting(*i).is_some_and(|s| s.muted));
        if !self.mix_down.unwrap_or(true) {
            return kept.map(AudioOutput::Track).collect();
        }
        let (mixed, separate): (Vec<usize>, Vec<usize>) = kept.partition(|i| setting(*i).is_none_or(|s| s.mix));
        let mut outputs = match mixed.len() {
            0 => Vec::new(),
            1 => vec![AudioOutput::Track(mixed[0])],
            _ => vec![AudioOutput::Mix(mixed)],
        };
        outputs.extend(separate.into_iter().map(AudioOutput::Track));
        outputs
    }
    /// The plan the engine builds from, for a clip whose tracks carry `sources` (in
    /// `audio_indices` order; empty or mismatched = no sources): every app track another track
    /// already carries (`app_already_carried`) stays out of the Mix and follows it as its own
    /// track, so merging never plays it twice. The editor still shows it merging.
    pub fn with_carried_apps_apart(&self, audio_indices: &[usize], sources: &[String]) -> EditPlan {
        let mut plan = self.clone();
        if sources.len() != audio_indices.len() {
            return plan;
        }
        for (at, &stream_index) in audio_indices.iter().enumerate() {
            if !app_already_carried(sources, at) {
                continue;
            }
            match plan.audio.iter_mut().find(|s| s.stream_index == stream_index) {
                Some(setting) => setting.mix = false,
                None => plan.audio.push(AudioSetting { stream_index, gain_db: 0.0, muted: false, mix: false }),
            }
        }
        plan
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

/// Which video property to favor under a file size limit.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ExportPriority {
    #[default]
    Manual,
    Motion,
    Detail,
}

impl ExportPriority {
    fn is_manual(&self) -> bool {
        *self == Self::Manual
    }
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
    #[serde(default, skip_serializing_if = "ExportPriority::is_manual")]
    pub priority: ExportPriority,
    /// Upscaled export height (re-encode only): the one opt-in exception to "never above the
    /// recording". Honoured only for a clip with a `NATIVE_HEIGHT_TAG` above its recorded
    /// height, and never above that tag. Old projects load as None.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub upscale_height: Option<u32>,
}

/// Smallest output height an export accepts.
pub const MIN_EXPORT_HEIGHT: u32 = 144;

impl ExportSettings {
    pub fn active_priority(&self) -> ExportPriority {
        if self.mode == ExportMode::Reencode && self.max_bytes.is_some() {
            self.priority
        } else {
            ExportPriority::Manual
        }
    }

    pub fn output_height_limit(&self) -> Option<u32> {
        if self.active_priority() == ExportPriority::Manual { self.height } else { None }
    }

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
    /// Where the recorder took this track from (`SOURCE_*`, `app:<exe>`), from the clip's
    /// `AUDIO_SOURCES_TAG`; "" for clips without it (older recordings, other apps' files).
    #[serde(default)]
    pub source: String,
}

/// File-level MP4 tag (written with `movflags=use_metadata_tags`) naming each audio track's
/// source as a JSON array, in audio-stream order. MP4 keeps no custom per-stream tags, only
/// `handler_name`, which stays the track's name.
#[allow(dead_code)]
pub const AUDIO_SOURCES_TAG: &str = "recall_audio_sources";
/// File-level MP4 tag: the source's own height when the clip was recorded below it (the
/// Recording resolution setting). Missing on clips recorded at native size and older ones.
#[allow(dead_code)]
pub const NATIVE_HEIGHT_TAG: &str = "recall_native_height";

/// Longest side an upscaled export may have. Exports pick their encoder by size
/// (`export/video_encoder.rs`, `nvenc_codec`): H.264 up to `H264_EXPORT_MAX_SIDE`, HEVC up to
/// this, so an upscale past 4096 is saved as HEVC rather than failing. The export side checks
/// at compile time that its table matches these two numbers.
#[allow(dead_code)]
pub const MAX_UPSCALE_SIDE: u32 = 8192;
/// Longest side an export stays H.264 at; the export page says when an upscale passes it.
#[allow(dead_code)]
pub const H264_EXPORT_MAX_SIDE: u32 = 4096;

/// Heights the export panel may offer under "Upscale": the standard steps above the
/// recording, up to and including native, leaving out any whose width (in the clip's shape)
/// would pass `MAX_UPSCALE_SIDE`. Every height is even (4:2:0 video needs even sides): an odd
/// native height (a windowed game) is offered one line lower, and only above the recording.
/// Empty for native clips.
#[allow(dead_code)]
pub fn upscale_choices(recorded_width: u32, recorded_height: u32, native_height: u32) -> Vec<u32> {
    let native_height = native_height & !1;
    if recorded_height == 0 || recorded_height >= native_height {
        return Vec::new();
    }
    let fits = |h: u32| h <= MAX_UPSCALE_SIDE && (recorded_width as u64 * h as u64).div_ceil(recorded_height as u64) <= MAX_UPSCALE_SIDE as u64;
    let mut out: Vec<u32> = [720u32, 1080, 1440, 2160].into_iter().filter(|h| *h > recorded_height && *h < native_height).collect();
    out.push(native_height);
    out.retain(|h| fits(*h));
    out
}

/// The height in a clip's `NATIVE_HEIGHT_TAG`, when it is present and readable.
#[allow(dead_code)]
pub fn native_height_from(tag: Option<&str>) -> Option<u32> {
    tag?.trim().parse::<u32>().ok().filter(|h| *h > 0)
}
/// The game's own process tree (process loopback).
#[allow(dead_code)]
pub const SOURCE_GAME_PROCESS: &str = "game-process";
/// Everything an output plays (endpoint loopback): holds every app, captured ones too.
#[allow(dead_code)]
pub const SOURCE_DESKTOP: &str = "desktop-loopback";
#[allow(dead_code)]
pub const SOURCE_MIC: &str = "mic";
#[allow(dead_code)]
pub const SOURCE_VOICE: &str = "voice-app";
/// An exported mix-down track (several sources summed).
#[allow(dead_code)]
pub const SOURCE_MIX: &str = "mix";
/// `app:<exe>`: an app the person chose to record as its own track.
#[allow(dead_code)]
pub const APP_SOURCE_PREFIX: &str = "app:";

/// The sources in a clip's `AUDIO_SOURCES_TAG`; empty when it is missing or unreadable.
#[allow(dead_code)]
pub fn parse_audio_sources(tag: Option<&str>) -> Vec<String> {
    tag.and_then(|t| serde_json::from_str::<Vec<String>>(t).ok()).unwrap_or_default()
}

/// The sources for a clip with `tracks` audio tracks; a tag that names a different number of
/// tracks (edited by another tool) counts as no tag.
#[allow(dead_code)]
pub fn audio_sources_for(tag: Option<&str>, tracks: usize) -> Vec<String> {
    let sources = parse_audio_sources(tag);
    if sources.len() == tracks { sources } else { Vec::new() }
}

/// The tag an export writes for its audio `outputs` (in output order): a mix is `SOURCE_MIX`,
/// a kept track keeps its source. `None` when the input had no usable tag.
#[allow(dead_code)]
pub fn export_sources(sources: &[String], audio_indices: &[usize], outputs: &[AudioOutput]) -> Option<Vec<String>> {
    if sources.is_empty() || sources.len() != audio_indices.len() {
        return None;
    }
    let of = |index: &usize| audio_indices.iter().position(|i| i == index).map(|n| sources[n].clone());
    outputs
        .iter()
        .map(|output| match output {
            AudioOutput::Mix(_) => Some(SOURCE_MIX.to_owned()),
            AudioOutput::Track(index) => of(index),
        })
        .collect()
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ClipInfo {
    pub path: String, pub duration: f64, pub width: u32, pub height: u32,
    pub video_fps: f64, pub source_bytes: u64, pub source_modified_ms: u64,
    pub audio: Vec<AudioTrackInfo>,
    /// Video keyframe times in seconds on the editor timeline (ascending). A
    /// lossless cut snaps its start to the last keyframe at or before it.
    #[serde(default)]
    pub keyframes: Vec<f64>,
    /// The source's own height when the clip was recorded below it (`NATIVE_HEIGHT_TAG`):
    /// the export page offers upscaled sizes up to it. None for native and older clips.
    #[serde(default)]
    pub native_height: Option<u32>,
}

#[allow(dead_code)] // used by recall-native's editor; the engine snaps inside the export
impl ClipInfo {
    /// The keyframe a lossless cut starting at `seconds` begins on.
    pub fn snap_to_keyframe(&self, seconds: f64) -> f64 { snap_to_keyframe(&self.keyframes, seconds) }
    /// The editor's starting setting for `track`: in the mix, unmuted, at 0 dB, whatever its
    /// source (game, mic, voice chat, an app, desktop audio). The engine reads a missing
    /// setting the same way.
    pub fn default_setting(&self, track: &AudioTrackInfo) -> AudioSetting {
        AudioSetting { stream_index: track.stream_index, gain_db: 0.0, muted: false, mix: true }
    }
    /// Every track's starting setting (`ClipInfo::default_setting`).
    pub fn default_audio(&self) -> Vec<AudioSetting> { self.audio.iter().map(|t| self.default_setting(t)).collect() }
    /// `track` is an app whose sound another track of this clip already carries
    /// (`app_already_carried`).
    pub fn carried_app(&self, track: &AudioTrackInfo) -> bool {
        let sources: Vec<String> = self.audio.iter().map(|t| t.source.clone()).collect();
        self.audio.iter().position(|t| t.stream_index == track.stream_index).is_some_and(|at| app_already_carried(&sources, at))
    }
    /// 0.1.11's starting setting for `track`, for a saved edit that has none for it: in the
    /// mix, except an app track another track already carries. Old edits then export what
    /// that editor showed for them.
    pub fn saved_default_setting(&self, track: &AudioTrackInfo) -> AudioSetting {
        AudioSetting { mix: !self.carried_app(track), ..self.default_setting(track) }
    }
}

/// The source at `position` is an app recorded as its own track (`app:<exe>`) whose sound
/// another track already carries: Desktop audio records every app, and an earlier export's
/// Mix track may hold it. Merging it again would play it twice.
pub fn app_already_carried(sources: &[String], position: usize) -> bool {
    sources.get(position).is_some_and(|s| s.starts_with(APP_SOURCE_PREFIX)) && sources.iter().any(|s| s == SOURCE_DESKTOP || s == SOURCE_MIX)
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
#[path = "edit_model_tests.rs"]
mod tests;
