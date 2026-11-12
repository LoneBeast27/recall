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
    /// Mixed into the export's first track with the other mixed tracks (default on, so
    /// players that play only track 1 still hear every source). Off: kept as its own track.
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
    /// Mix tracks into track 1 ("Mix") or export every track separately. `None` (edits saved
    /// before the choice existed, or never changed): the editor's remembered default; the
    /// engine reads `None` as on.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mix_down: Option<bool>,
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
    /// The audio tracks an export writes, first to last: the mixed tracks as one "Mix" track
    /// (a lone mixed track stays itself), then every other kept track in stream order. Muted
    /// tracks are dropped. A stream with no setting is an untouched track of its own.
    pub fn audio_outputs(&self, audio_indices: &[usize]) -> Vec<AudioOutput> {
        let setting = |index: usize| self.audio.iter().find(|s| s.stream_index == index);
        let kept = audio_indices.iter().copied().filter(|i| !setting(*i).is_some_and(|s| s.muted));
        if !self.mix_down.unwrap_or(true) {
            // Mixing off: every kept track on its own, in stream order (the switches keep their values).
            return kept.map(AudioOutput::Track).collect();
        }
        let (mixed, separate): (Vec<usize>, Vec<usize>) = kept.partition(|i| setting(*i).is_some_and(|s| s.mix));
        let mut outputs = match mixed.len() {
            0 => Vec::new(),
            1 => vec![AudioOutput::Track(mixed[0])],
            _ => vec![AudioOutput::Mix(mixed)],
        };
        outputs.extend(separate.into_iter().map(AudioOutput::Track));
        outputs
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
/// would pass `MAX_UPSCALE_SIDE`. Empty for native clips.
#[allow(dead_code)]
pub fn upscale_choices(recorded_width: u32, recorded_height: u32, native_height: u32) -> Vec<u32> {
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
/// An exported mix-down track (several sources summed): in the mix, no hint.
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

#[allow(dead_code)] // the editor (recall-native) reads these; the engine tests use them
impl AudioTrackInfo {
    /// An app the person recorded as its own track (it is also inside Desktop audio).
    pub fn is_app(&self) -> bool { self.source.starts_with(APP_SOURCE_PREFIX) }
    /// The editor's starting setting: in the mix, except app tracks, which Desktop audio
    /// (or the game) already carries. Clips without sources keep everything in the mix.
    pub fn default_setting(&self) -> AudioSetting {
        AudioSetting { stream_index: self.stream_index, gain_db: 0.0, muted: false, mix: !self.is_app() }
    }
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
    /// The game track recorded an output (Desktop mode), so it already holds every app.
    pub fn desktop_loopback(&self) -> bool { self.audio.iter().any(|t| t.source == SOURCE_DESKTOP) }
    /// The line under a track's mix switch, when mixing it would play it twice: an app track in
    /// a Desktop clip, or in an exported clip whose Mix track may already hold it.
    pub fn mix_hint(&self, stream_index: usize, mix: bool) -> Option<&'static str> {
        let app = self.audio.iter().any(|t| t.stream_index == stream_index && t.is_app());
        if !(mix && app) {
            return None;
        }
        if self.desktop_loopback() {
            return Some("This app is also in Desktop audio, so mixing it doubles it.");
        }
        self.audio.iter().any(|t| t.source == SOURCE_MIX).then_some("This app may already be in the Mix track, so mixing it again can double it.")
    }
    /// Every track's starting setting (`AudioTrackInfo::default_setting`).
    pub fn default_audio(&self) -> Vec<AudioSetting> { self.audio.iter().map(AudioTrackInfo::default_setting).collect() }
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

    fn setting(stream_index: usize, muted: bool, mix: bool) -> AudioSetting {
        AudioSetting { stream_index, gain_db: 0.0, muted, mix }
    }

    #[test]
    fn mixed_tracks_come_first_as_one_and_muted_tracks_are_dropped() {
        let plan = |audio| EditPlan { mix_down: None, ranges: vec![TimeRange::new(0.0, 1.0)], audio };
        let both = plan(vec![setting(1, false, true), setting(2, false, true)]);
        assert_eq!(both.audio_outputs(&[1, 2]), vec![AudioOutput::Mix(vec![1, 2])]);
        let one = plan(vec![setting(1, false, false), setting(2, false, true)]);
        assert_eq!(one.audio_outputs(&[1, 2]), vec![AudioOutput::Track(2), AudioOutput::Track(1)], "a lone mixed track leads");
        let muted = plan(vec![setting(1, false, true), setting(2, true, true), setting(3, false, true)]);
        assert_eq!(muted.audio_outputs(&[1, 2, 3]), vec![AudioOutput::Mix(vec![1, 3])]);
        assert_eq!(plan(vec![]).audio_outputs(&[1, 2]), vec![AudioOutput::Track(1), AudioOutput::Track(2)], "no setting: untouched");
        let saved: AudioSetting = serde_json::from_str(r#"{"stream_index":1,"gain_db":0.0,"muted":false}"#).unwrap();
        assert!(saved.mix, "edits saved before the choice existed mix by default");
    }

    fn info(sources: &[&str]) -> ClipInfo {
        let audio = sources
            .iter()
            .enumerate()
            .map(|(i, s)| AudioTrackInfo { stream_index: i + 1, name: format!("T{i}"), channels: 2, sample_rate: 48_000, preview_path: String::new(), waveform: Vec::new(), source: (*s).into() })
            .collect();
        ClipInfo { path: "a.mp4".into(), duration: 1.0, width: 4, height: 2, video_fps: 60.0, source_bytes: 1, source_modified_ms: 1, audio, keyframes: Vec::new(), ..Default::default() }
    }

    #[test]
    fn app_tracks_start_out_of_the_mix_and_untagged_clips_keep_everything_in() {
        let tagged = info(&[SOURCE_DESKTOP, SOURCE_MIC, "app:a.exe"]);
        assert_eq!(tagged.default_audio().iter().map(|s| s.mix).collect::<Vec<_>>(), [true, true, false]);
        assert_eq!(tagged.mix_hint(3, true), Some("This app is also in Desktop audio, so mixing it doubles it."));
        assert_eq!(tagged.mix_hint(3, false), None, "no hint while it is out of the mix");
        assert_eq!(tagged.mix_hint(1, true), None, "only app tracks");
        let game = info(&[SOURCE_GAME_PROCESS, "app:a.exe"]);
        assert_eq!(game.mix_hint(2, true), None, "a game's own track holds no other app");
        let old = info(&["", ""]);
        assert!(old.default_audio().iter().all(|s| s.mix), "clips without sources: today's behaviour");
        assert_eq!(parse_audio_sources(Some(r#"["desktop-loopback","app:a.exe"]"#)), ["desktop-loopback", "app:a.exe"]);
        let exported = info(&[SOURCE_MIX, "app:a.exe"]);
        assert!(exported.default_audio()[0].mix && exported.mix_hint(1, true).is_none(), "an exported mix: in the mix, no hint");
        assert!(!exported.default_audio()[1].mix, "the app track of an exported clip still starts out of the mix");
        assert_eq!(exported.mix_hint(2, true), Some("This app may already be in the Mix track, so mixing it again can double it."));
        assert_eq!(exported.mix_hint(2, false), None);
        assert!(audio_sources_for(Some(r#"["mic"]"#), 2).is_empty(), "a tag for another track count is no tag");
        assert_eq!(audio_sources_for(Some(r#"["mic","app:a.exe"]"#), 2), ["mic", "app:a.exe"]);
        assert!(parse_audio_sources(Some("not json")).is_empty() && parse_audio_sources(None).is_empty());
    }

    #[test]
    fn with_mixing_off_every_kept_track_is_its_own_track() {
        let audio = vec![
            AudioSetting { stream_index: 1, gain_db: 0.0, muted: false, mix: true },
            AudioSetting { stream_index: 2, gain_db: 0.0, muted: false, mix: true },
            AudioSetting { stream_index: 3, gain_db: 0.0, muted: true, mix: true },
        ];
        let mut plan = EditPlan { ranges: vec![TimeRange::new(0.0, 1.0)], audio, mix_down: Some(false) };
        assert_eq!(plan.audio_outputs(&[1, 2, 3]), vec![AudioOutput::Track(1), AudioOutput::Track(2)], "muted still dropped");
        plan.mix_down = None;
        assert_eq!(plan.audio_outputs(&[1, 2, 3]), vec![AudioOutput::Mix(vec![1, 2])], "no choice: on");
        let old: EditPlan = serde_json::from_str(r#"{"ranges":[{"start":0.0,"end":1.0}]}"#).unwrap();
        assert_eq!(old.mix_down, None, "saved before the choice existed");
        assert!(!serde_json::to_string(&old).unwrap().contains("mix_down"), "None is not written");
    }

    #[test]
    fn repeated_source_kinds_each_get_their_own_default() {
        // The source manager (feat/audio-sources) may record several outputs and mics.
        let tag = r#"["game-process","desktop-loopback","desktop-loopback","mic","mic","app:x.exe"]"#;
        let sources = audio_sources_for(Some(tag), 6);
        assert_eq!(sources.len(), 6, "count matching takes repeats as they are");
        assert!(audio_sources_for(Some(tag), 5).is_empty());
        let refs: Vec<&str> = sources.iter().map(String::as_str).collect();
        let clip = info(&refs);
        assert_eq!(clip.default_audio().iter().map(|s| s.mix).collect::<Vec<_>>(), [true, true, true, true, true, false]);
        assert_eq!(clip.mix_hint(6, true), Some("This app is also in Desktop audio, so mixing it doubles it."), "any Desktop track");
        let exported = info(&["mix", "mic", "app:x.exe", "app:y.exe"]);
        assert_eq!(exported.default_audio().iter().map(|s| s.mix).collect::<Vec<_>>(), [true, true, false, false]);
        assert!(exported.mix_hint(3, true).is_some() && exported.mix_hint(4, true).is_some(), "every app track, with a Mix track");
    }

    #[test]
    fn an_export_tags_its_own_tracks_in_output_order() {
        let sources = vec![SOURCE_DESKTOP.to_owned(), SOURCE_MIC.to_owned(), "app:a.exe".to_owned()];
        let outputs = [AudioOutput::Mix(vec![1, 2]), AudioOutput::Track(3)];
        assert_eq!(export_sources(&sources, &[1, 2, 3], &outputs), Some(vec![SOURCE_MIX.into(), "app:a.exe".into()]));
        assert_eq!(export_sources(&sources, &[1, 2, 3], &[AudioOutput::Track(3), AudioOutput::Track(1)]), Some(vec!["app:a.exe".into(), SOURCE_DESKTOP.into()]));
        assert_eq!(export_sources(&[], &[1, 2], &[AudioOutput::Track(1)]), None, "untagged input: untagged output");
        assert_eq!(export_sources(&sources, &[1, 2], &[AudioOutput::Track(1)]), None, "count mismatch: untagged");
    }

    #[test]
    fn duration_is_output_length_after_speed() {
        let plan = EditPlan { mix_down: None, ranges: vec![TimeRange { start: 0.0, end: 2.0, speed: 0.5 }, TimeRange { start: 3.0, end: 5.0, speed: 2.0 }], audio: vec![] };
        assert!((plan.duration() - 5.0).abs() < 1e-9);
        assert!((plan.source_duration() - 4.0).abs() < 1e-9);
        assert!(plan.validate(6.0, &[]).is_ok());
    }

    #[test]
    fn speed_and_lossless_combinations_are_validated() {
        let mut plan = EditPlan { mix_down: None, ranges: vec![TimeRange { start: 0.0, end: 1.0, speed: 0.2 }], audio: vec![] };
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
        let settings = ExportSettings { mode: ExportMode::Lossless, ..ExportSettings::default() };
        let text = serde_json::to_string(&settings).unwrap();
        assert_eq!(text, r#"{"mode":"lossless","max_bytes":null,"height":null}"#);
        assert_eq!(serde_json::from_str::<ExportSettings>(&text).unwrap(), settings);
    }
}
