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
/// `dynamic`: the island finds the game's HUD and takes a free corner or edge.
pub const CORNERS: [&str; 5] = ["dynamic", "top-left", "top-right", "bottom-left", "bottom-right"];
/// `overlay.corner_version`: 2 = the corner is honoured (before, the island
/// ignored it and kept a per-game spot, so older configs load as `dynamic`).
pub const CORNER_VERSION: u32 = 2;
/// Accepted replay-buffer lengths (seconds). The ring lives in RAM.
pub const BUFFER_SECS_MIN: u32 = 10;
pub const BUFFER_SECS_MAX: u32 = 600;
/// `recording.codec` tokens (`RECALL_CODEC`).
pub const CODECS: [&str; 3] = ["h264", "hevc", "av1"];
/// `recording.quality` tokens (`RECALL_QUALITY`).
pub const QUALITIES: [&str; 3] = ["performance", "balanced", "quality"];
pub const FPS_MIN: u32 = 15;
/// The recording rate is also the capture cap (1.1x): above 240 a fast game (VALORANT ~750 fps)
/// would be copied and encoded nearly every frame it draws, the cost behind the 10-26 freezes.
/// Exports keep their own higher rates. (Review 11-05; Rohan's call between 240 and 960 pending.)
pub const FPS_MAX: u32 = 240;
pub const BITRATE_MBPS_MIN: u32 = 1;
pub const BITRATE_MBPS_MAX: u32 = 200;
/// Smallest non-zero `storage.cap_gb`: a tiny cap would recycle nearly every
/// clip on the first run.
pub const STORAGE_CAP_MIN_GB: u32 = 20;

/// One save-length preset: `seconds` of the buffer (0 = all of it).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SaveHotkey {
    pub hotkey: String,
    #[serde(default)]
    pub seconds: u32,
}

/// `[recording]` -- what the unified record mode captures and how.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct RecordingCfg {
    /// Detect the foreground fullscreen game and record it automatically.
    pub auto_record: bool,
    /// Folder the engine writes clips into (`RECALL_OUT_DIR`).
    pub output_dir: String,
    /// Replay-buffer length in seconds; also the full-buffer save length.
    pub buffer_secs: u32,
    /// Legacy single save accelerator. Mirrors the first full-buffer entry of
    /// `save_hotkeys`; setting it (patch, env, a config without
    /// `save_hotkeys`) moves that entry (`record_hotkeys`).
    pub save_hotkey: String,
    /// Global accelerator that toggles the recorded microphone.
    pub mic_hotkey: String,
    /// Mark "now" for the clips saved later ("" = off).
    pub bookmark_hotkey: String,
    /// PNG of the live frame into `<output_dir>\screenshots` ("" = off).
    pub screenshot_hotkey: String,
    /// Start / stop the manual long recording ("" = off).
    pub record_hotkey: String,
    /// Show / hide the performance-stats island (`overlay.stats`; "" = off).
    pub stats_hotkey: String,
    /// `h264` | `hevc` | `av1` (`RECALL_CODEC`).
    pub codec: String,
    /// Nominal capture/encode rate (`RECALL_FPS`).
    pub fps: u32,
    /// Video bitrate in Mbit/s (`RECALL_BITRATE`, sent in bit/s).
    pub bitrate_mbps: u32,
    /// `performance` | `balanced` | `quality` (`RECALL_QUALITY`).
    pub quality: String,
    /// Exe captured as its own "Voice" track and kept out of Game, e.g.
    /// `chat.exe` ("" = off). Read only to build `audio_sources`.
    pub voice_app: String,
    /// Hex CPU affinity mask applied to the controller and every child.
    pub cpu_mask: String,
    /// With no game target, capture the primary monitor as "Desktop".
    pub record_desktop_when_idle: bool,
    /// Capture endpoint for the Mic track ("" = the Windows default mic;
    /// `RECALL_AUDIO_MIC`). Exact friendly name as Windows shows it.
    pub mic_device: String,
    /// Record a Mic track at all (`RECALL_AUDIO_MIC_OFF=1` when false).
    pub mic_enabled: bool,
    /// Render endpoint recorded as Game in Desktop mode ("" = the Windows
    /// default output; `RECALL_AUDIO_GAME`). Games record their own process.
    pub desktop_audio_device: String,
    /// Every recorded source, in track order (`RECALL_AUDIO_SOURCES`; the Audio page).
    /// A table array: TOML writes it after the plain keys. Built from the old audio keys
    /// above when a config has none (`record_audio_sources::migrate_loaded`).
    pub audio_sources: Vec<crate::record_audio_sources::AudioSource>,
    /// Old app list, read only to build `audio_sources` (`record_audio_sources::from_legacy`).
    pub app_audio: Vec<crate::record_app_audio::AppAudioSource>,
    /// Save-length presets. Last field: TOML writes it as
    /// `[[recording.save_hotkeys]]` tables after the plain keys.
    pub save_hotkeys: Vec<SaveHotkey>,
}

impl Default for RecordingCfg {
    fn default() -> Self {
        RecordingCfg {
            auto_record: true,
            output_dir: default_output_dir(),
            buffer_secs: 120,
            save_hotkey: "Alt+N".into(),
            mic_hotkey: "Alt+M".into(),
            // One direct Alt combo per action (Rohan, 2026-11-05: the old
            // two-step chord was Alt too, so it bought nothing).
            bookmark_hotkey: crate::record_hotkeys::DEFAULT_BOOKMARK.into(),
            screenshot_hotkey: crate::record_hotkeys::DEFAULT_SCREENSHOT.into(),
            record_hotkey: crate::record_hotkeys::DEFAULT_RECORD.into(),
            stats_hotkey: crate::record_hotkeys::DEFAULT_STATS.into(),
            codec: "h264".into(),
            fps: 60,
            bitrate_mbps: 25,
            quality: "balanced".into(),
            voice_app: String::new(),
            cpu_mask: "auto".into(),
            record_desktop_when_idle: false,
            mic_device: String::new(),
            mic_enabled: true,
            desktop_audio_device: String::new(),
            audio_sources: crate::record_audio_sources::default_sources(),
            app_audio: Vec::new(),
            save_hotkeys: crate::record_hotkeys::default_save_hotkeys(),
        }
    }
}

/// `<Videos>\Recall`: the Videos known folder (follows a moved or OneDrive Videos), else
/// `%USERPROFILE%\Videos\Recall`.
pub fn default_output_dir() -> String {
    let videos = videos_folder().unwrap_or_else(|| PathBuf::from(std::env::var_os("USERPROFILE").unwrap_or_default()).join("Videos"));
    videos.join("Recall").to_string_lossy().into_owned()
}

fn videos_folder() -> Option<PathBuf> {
    use windows::Win32::System::Com::CoTaskMemFree;
    use windows::Win32::UI::Shell::{FOLDERID_Videos, SHGetKnownFolderPath, KF_FLAG_DEFAULT};
    // SAFETY: the returned buffer is copied, then freed with the allocator that made it.
    unsafe {
        let raw = SHGetKnownFolderPath(&FOLDERID_Videos, KF_FLAG_DEFAULT, None).ok()?;
        let path = raw.to_string().ok();
        CoTaskMemFree(Some(raw.0 as *const _));
        path.filter(|p| !p.is_empty()).map(PathBuf::from)
    }
}

impl RecordingCfg {
    /// Seconds a preset saves: 0 = the whole buffer, never more than it.
    pub fn save_seconds(&self, preset: u32) -> u32 {
        if preset == 0 {
            self.buffer_secs
        } else {
            preset.min(self.buffer_secs)
        }
    }
}

/// `[storage]` -- automatic clean-up of the clip folder (`record_storage`).
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct StorageCfg {
    /// GiB of top-level Recall clips allowed in `recording.output_dir`; the
    /// oldest unprotected clips go to the Recycle Bin past it. 0 = off, else
    /// at least [`STORAGE_CAP_MIN_GB`].
    pub cap_gb: u32,
}

/// `[detect]` -- game detection on the record path (`record_target`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct DetectCfg {
    /// Exe stems (`Discord` or `Discord.exe`, any case) whose fullscreen windows
    /// are games with no render check, even when a deny list names them.
    /// Windowed, they are not targets. A `none` override and Recall's own
    /// processes still win.
    pub fullscreen_games: Vec<String>,
}

impl Default for DetectCfg {
    fn default() -> Self {
        // Rohan, 2026-11-05: "discord i dont mind being in there as a personal decision".
        DetectCfg { fullscreen_games: vec!["Discord".into()] }
    }
}

/// `overlay.stats_style` tokens (more gauge styles later).
pub const STATS_STYLES: [&str; 1] = ["minimal"];

/// `[overlay]` -- the controller-owned `recall-overlay.exe` (Recall Island):
/// notices (`enabled`) and the performance-stats island (`stats`). The
/// process runs while either is on.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct OverlayCfg {
    /// Show notices (saves, bookmarks, recording changes).
    pub enabled: bool,
    pub corner: String,
    /// [`CORNER_VERSION`] once `corner` means what it says (see `migrate_loaded`).
    pub corner_version: u32,
    /// Show the performance-stats island (FPS via Windows frame timing, GPU, CPU).
    pub stats: bool,
    /// Gauge style of the stats island (`STATS_STYLES`).
    pub stats_style: String,
}

impl Default for OverlayCfg {
    fn default() -> Self {
        OverlayCfg { enabled: false, corner: "dynamic".into(), corner_version: CORNER_VERSION, stats: false, stats_style: "minimal".into() }
    }
}

impl OverlayCfg {
    /// A config saved before the corner was honoured always said `top-right`
    /// (the default) while the island sat at its per-game spot: it loads as
    /// `dynamic`, which keeps that behaviour. Saved again, it is version 2.
    pub fn migrate_loaded(&mut self, toml_text: &str) {
        let version = toml::from_str::<toml::Value>(toml_text)
            .ok()
            .and_then(|v| v.get("overlay")?.get("corner_version")?.as_integer())
            .unwrap_or(0);
        if version < CORNER_VERSION as i64 {
            self.corner = "dynamic".into();
            self.corner_version = CORNER_VERSION;
        }
    }
}

impl OverlayCfg {
    /// The overlay process is wanted for notices or stats.
    pub fn wants_process(&self) -> bool {
        self.enabled || self.stats
    }
}

/// `recording.cpu_mask = "auto"`: no CPUs named, so [`fit_cpu_mask`] picks the high half.
pub const AUTO_CPU_MASK: usize = 0;

/// The CPUs of `wanted` that exist in `system`; when none do ("auto", or ffff0000 on a
/// machine with 16 or fewer threads), the high half of `system`; `None` (no pin) with 4 or
/// fewer CPUs, where a pin would only crowd the game.
pub fn fit_cpu_mask(wanted: usize, system: usize) -> Option<usize> {
    let cpus = system.count_ones();
    if cpus <= 4 {
        return None;
    }
    if wanted & system != 0 {
        return Some(wanted & system);
    }
    let (mut high, mut seen) = (0usize, 0u32);
    for bit in (0..usize::BITS).rev() {
        if system & (1 << bit) != 0 && seen < cpus / 2 {
            high |= 1 << bit;
            seen += 1;
        }
    }
    Some(high)
}

/// Parse a hex affinity mask (`"ffff0000"` / `"0xffff0000"`) or `"auto"`
/// ([`AUTO_CPU_MASK`]); zero is invalid.
pub fn parse_cpu_mask(text: &str) -> Result<usize, String> {
    let raw = text.trim();
    if raw.eq_ignore_ascii_case("auto") {
        return Ok(AUTO_CPU_MASK);
    }
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
    crate::record_hotkeys::validate(rec)?;
    validate_quality(rec)?;
    let midi = &cfg.midi.input;
    if midi.len() > 256 || midi.chars().any(char::is_control) {
        return Err("midi.input must be a MIDI port name (at most 256 characters, one line)".into());
    }
    if let Some(bad) = cfg.detect.fullscreen_games.iter().find(|g| g.trim().is_empty() || g.contains(['\\', '/'])) {
        return Err(format!("detect.fullscreen_games entry '{bad}' must be an exe name like Discord.exe (no folders)"));
    }
    let cap = cfg.storage.cap_gb;
    if (1..STORAGE_CAP_MIN_GB).contains(&cap) {
        return Err(format!("storage.cap_gb must be 0 (off) or at least {STORAGE_CAP_MIN_GB} GiB, got {cap}"));
    }
    if !CORNERS.contains(&cfg.overlay.corner.as_str()) {
        return Err(format!(
            "overlay.corner '{}' must be one of {}",
            cfg.overlay.corner,
            CORNERS.join(" | ")
        ));
    }
    if !STATS_STYLES.contains(&cfg.overlay.stats_style.as_str()) {
        return Err(format!(
            "overlay.stats_style '{}' must be one of {}",
            cfg.overlay.stats_style,
            STATS_STYLES.join(" | ")
        ));
    }
    Ok(())
}

/// Codec / rate / bitrate / quality tokens, audio device names and the
/// voice-chat exe name.
fn validate_quality(rec: &RecordingCfg) -> Result<(), String> {
    if !CODECS.contains(&rec.codec.as_str()) {
        return Err(format!("recording.codec '{}' must be one of {}", rec.codec, CODECS.join(" | ")));
    }
    if !(FPS_MIN..=FPS_MAX).contains(&rec.fps) {
        return Err(format!("recording.fps must be {FPS_MIN}..={FPS_MAX}, got {}", rec.fps));
    }
    if !(BITRATE_MBPS_MIN..=BITRATE_MBPS_MAX).contains(&rec.bitrate_mbps) {
        return Err(format!(
            "recording.bitrate_mbps must be {BITRATE_MBPS_MIN}..={BITRATE_MBPS_MAX}, got {}",
            rec.bitrate_mbps
        ));
    }
    if !QUALITIES.contains(&rec.quality.as_str()) {
        return Err(format!("recording.quality '{}' must be one of {}", rec.quality, QUALITIES.join(" | ")));
    }
    for (key, name) in [("mic_device", &rec.mic_device), ("desktop_audio_device", &rec.desktop_audio_device)] {
        if name.len() > 256 || name.chars().any(char::is_control) {
            return Err(format!("recording.{key} must be an audio device name (at most 256 characters, one line)"));
        }
    }
    crate::record_app_audio::validate(&rec.app_audio)?;
    crate::record_audio_sources::validate(&rec.audio_sources)?;
    let voice = rec.voice_app.trim();
    if voice.len() > 64 || voice.chars().any(|c| matches!(c, '\\' | '/' | ':') || c.is_control()) {
        return Err(format!("recording.voice_app '{voice}' must be an exe name like chat.exe (no folders)"));
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
    // A legacy `recording.save_hotkey` moves the full-buffer preset.
    let patch = crate::record_hotkeys::rewrite_legacy_patch(&base.recording, patch);
    let mut merged = before.clone();
    merge_json(&mut merged, &patch);
    let mut config: Config = serde_json::from_value(merged).map_err(|e| format!("invalid config: {e}"))?;
    config.recording.save_hotkey = crate::record_hotkeys::legacy_save_hotkey(&config.recording.save_hotkeys);
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
#[path = "record_config_tests.rs"]
mod tests;
