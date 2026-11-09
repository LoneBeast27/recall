//! Config schema + TOML persistence (`%APPDATA%\recall-hdr\config.toml`) and the
//! `HKCU\...\Run` autostart mirror.
//!
//! The schema is design-frozen (Wave 1). It mirrors the frozen `types.rs`
//! contracts (`RuleKind`, `ClampMode`) rather than redefining them, so a config
//! `default_clamp = "punchy"` deserializes straight into the same [`ClampMode`]
//! the reconciler consumes.
//!
//! Persistence rules, all "never lose the user's display in a bad state":
//! - [`Config::load_or_default`] never panics. A damaged file is backed up to a
//!   dated `config.toml.bad-YYYY-MM-DD` (one a day, never overwritten); a bad
//!   value costs only its own key, a file that is not TOML (or is empty) falls
//!   back to `config.toml.last-good`, else [`Config::default`] (`config_io.rs`).
//! - [`Config::save`] is durable: write a temp file, `sync_all`, then rename over
//!   the real file, keeping the keys this build doesn't know (`durable.rs`).
//! - [`Config::save`] also mirrors `startup.autostart` into the `HKCU` Run key
//!   (best-effort -- a registry failure is logged, not fatal to the save).
//!
//! The Run-key mirror itself lives in `config_autostart.rs`.

#[path = "config_autostart.rs"]
mod autostart;
#[path = "config_io.rs"]
mod io;

use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::log;
use crate::record_config::{DetectCfg, OverlayCfg, RecordingCfg, StorageCfg};
use crate::types::{ClampMode, RuleKind};
pub use autostart::{sync_autostart, LEGACY_RUN_ARG, RECORD_RUN_ARG};

// ---------------------------------------------------------------------------
// Schema
// ---------------------------------------------------------------------------

/// Current on-disk schema version. Bumped only on a breaking layout change; the
/// loader tolerates a missing `version` (serde default) so hand-written configs
/// don't need it.
const SCHEMA_VERSION: u32 = 1;

/// Root config, mirroring `%APPDATA%\recall-hdr\config.toml`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Config {
    /// Schema version. Defaults to [`SCHEMA_VERSION`] when absent.
    #[serde(default = "default_version")]
    pub version: u32,
    #[serde(default)]
    pub display: DisplayCfg,
    #[serde(default)]
    pub behavior: BehaviorCfg,
    #[serde(default)]
    pub hotkey: HotkeyCfg,
    #[serde(default)]
    pub startup: StartupCfg,
    /// `[recording]` -- the unified record mode (see `record_config`).
    #[serde(default)]
    pub recording: RecordingCfg,
    /// `[overlay]` -- the controller-owned in-game toast process.
    #[serde(default)]
    pub overlay: OverlayCfg,
    /// `[storage]` -- clip-folder size cap + auto-clean (`record_storage`).
    #[serde(default)]
    pub storage: StorageCfg,
    /// `[midi]` -- the MIDI input the app's Audio page listens to ("" = off).
    #[serde(default)]
    pub midi: MidiCfg,
    /// `[detect]` -- record-path game detection (`record_target`).
    #[serde(default)]
    pub detect: DetectCfg,
    /// Per-exe rule overrides. Exe stems are matched case-insensitively.
    #[serde(default, rename = "overrides")]
    pub overrides: Vec<Override>,
}

/// `[midi]` -- read by the native app's Audio page only (the controller just
/// stores it). `input` is a MIDI input port name as Windows lists it.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct MidiCfg {
    pub input: String,
}

/// `[display]` -- which panel we manage.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct DisplayCfg {
    /// Friendly name (e.g. `"Monitor 27Q"`) used to select the target display;
    /// "" (the default) = the primary display (`display::resolve_target`).
    pub target_friendly_name: String,
    /// Optional exact device-path pin. Empty/absent -> select by friendly name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_device_path: Option<String>,
}

/// `[behavior]` -- default rule/clamp when no per-exe override matches.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BehaviorCfg {
    /// Rule applied to an unmatched foreground game.
    #[serde(with = "rulekind_snake")]
    pub default_game_class: RuleKind,
    /// Clamp intensity applied when a rule engages a clamp.
    #[serde(with = "clampmode_snake")]
    pub default_clamp: ClampMode,
    /// SDR-content white level (nits) while HDR is on. Conservative for ABL
    /// panels; the schema comment pins the sane band at 200-250.
    pub sdr_white_nits: u32,
    /// Foreground-change debounce (ms) before the reconciler acts.
    pub debounce_ms: u32,
    /// Unified record mode only: drive HDR/clamp/SDR-white automatically by game
    /// (the legacy reconciler). `false` (default) = the recorder never changes a
    /// display setting. The legacy `--legacy-hdr` mode ignores this flag.
    #[serde(default)]
    pub hdr_auto: bool,
}

/// `[hotkey]` -- user-facing accelerators (parsed by the tray layer, not here).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HotkeyCfg {
    /// Accelerator that toggles Windows HDR on the target display.
    pub toggle_hdr: String,
    /// Accelerator that saves the engine's replay buffer (`save_clip`).
    /// serde-defaulted so configs written before this field still parse
    /// (an unparseable config gets quarantined to .bad + silent defaults).
    #[serde(default = "default_save_clip_accel")]
    pub save_clip: String,
}

fn default_save_clip_accel() -> String {
    "Ctrl+Alt+S".into()
}

/// `[startup]` -- run-at-login behavior, mirrored to the HKCU Run key on save.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StartupCfg {
    /// Whether recall-hdr installs itself into the HKCU Run key.
    pub autostart: bool,
}

/// A single `[[overrides]]` entry: force a rule (and optionally a clamp) for one
/// executable, keyed by its lowercased file stem.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Override {
    /// Executable stem, lowercase (e.g. `"hdr-testcard"` for `HDR-Testcard.exe`).
    pub exe: String,
    /// Rule to force for this exe.
    #[serde(with = "rulekind_snake")]
    pub kind: RuleKind,
    /// Optional per-exe clamp override; falls back to `default_clamp` when absent.
    #[serde(
        default,
        with = "clampmode_snake_opt",
        skip_serializing_if = "Option::is_none"
    )]
    pub clamp: Option<ClampMode>,
}

// ---------------------------------------------------------------------------
// serde adapters: frozen enums <-> the design's snake_case TOML tokens
// ---------------------------------------------------------------------------
//
// `types.rs` is frozen and derives serde WITHOUT `#[serde(rename_all)]`, so the
// derived tokens are the PascalCase variant names (`SdrGame`, `Punchy`). The
// design-frozen config schema, however, pins snake_case tokens (`sdr_game`,
// `punchy`) that a user may hand-edit. We must not redefine or edit the frozen
// enums, so we bridge here: these `#[serde(with)]` adapters map each frozen enum
// to/from the exact snake_case tokens the schema doc specifies. The stored Rust
// type is still the frozen `RuleKind`/`ClampMode` -- only the wire tokens change.

mod rulekind_snake {
    use super::RuleKind;
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(v: &RuleKind, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(match v {
            RuleKind::HdrGame => "hdr_game",
            RuleKind::SdrGame => "sdr_game",
            RuleKind::None => "none",
        })
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<RuleKind, D::Error> {
        let s = String::deserialize(d)?;
        match s.as_str() {
            "hdr_game" => Ok(RuleKind::HdrGame),
            "sdr_game" => Ok(RuleKind::SdrGame),
            "none" => Ok(RuleKind::None),
            other => Err(serde::de::Error::custom(format!(
                "invalid rule kind '{other}' (expected hdr_game | sdr_game | none)"
            ))),
        }
    }
}

mod clampmode_snake {
    use super::ClampMode;
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(v: &ClampMode, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(token(v))
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<ClampMode, D::Error> {
        let s = String::deserialize(d)?;
        from_token(&s).map_err(serde::de::Error::custom)
    }

    pub(super) fn token(v: &ClampMode) -> &'static str {
        match v {
            ClampMode::Punchy => "punchy",
            ClampMode::Accurate => "accurate",
            ClampMode::Off => "off",
        }
    }

    pub(super) fn from_token(s: &str) -> Result<ClampMode, String> {
        match s {
            "punchy" => Ok(ClampMode::Punchy),
            "accurate" => Ok(ClampMode::Accurate),
            "off" => Ok(ClampMode::Off),
            other => Err(format!(
                "invalid clamp mode '{other}' (expected punchy | accurate | off)"
            )),
        }
    }
}

/// `Option<ClampMode>` variant of [`clampmode_snake`] for the per-exe override.
mod clampmode_snake_opt {
    use super::{clampmode_snake, ClampMode};
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(v: &Option<ClampMode>, s: S) -> Result<S::Ok, S::Error> {
        match v {
            Some(c) => s.serialize_str(clampmode_snake::token(c)),
            None => s.serialize_none(),
        }
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Option<ClampMode>, D::Error> {
        let opt = Option::<String>::deserialize(d)?;
        match opt {
            Some(s) => clampmode_snake::from_token(&s)
                .map(Some)
                .map_err(serde::de::Error::custom),
            None => Ok(None),
        }
    }
}

fn default_version() -> u32 {
    SCHEMA_VERSION
}

// ---------------------------------------------------------------------------
// Defaults (must match the schema block in the design)
// ---------------------------------------------------------------------------

impl Default for BehaviorCfg {
    fn default() -> Self {
        BehaviorCfg {
            default_game_class: RuleKind::SdrGame,
            default_clamp: ClampMode::Punchy,
            sdr_white_nits: 220,
            debounce_ms: 400,
            hdr_auto: false,
        }
    }
}

impl Default for HotkeyCfg {
    fn default() -> Self {
        HotkeyCfg {
            toggle_hdr: "Ctrl+Alt+H".into(),
            save_clip: default_save_clip_accel(),
        }
    }
}

impl Default for StartupCfg {
    fn default() -> Self {
        StartupCfg { autostart: false } // opt-in: nothing registers itself at login until the user turns it on
    }
}

impl Default for Config {
    fn default() -> Self {
        Config {
            version: SCHEMA_VERSION,
            display: DisplayCfg::default(),
            behavior: BehaviorCfg::default(),
            hotkey: HotkeyCfg::default(),
            startup: StartupCfg::default(),
            recording: RecordingCfg::default(),
            overlay: OverlayCfg::default(),
            storage: StorageCfg::default(),
            midi: MidiCfg::default(),
            detect: DetectCfg::default(),
            // No entries: the dev test card (hdr-testcard) showed as "Recorded" in every
            // install's Games list (final review F-detect-6); tools/e2e writes its own override.
            overrides: Vec::new(),
        }
    }
}

// ---------------------------------------------------------------------------
// Path / load / save
// ---------------------------------------------------------------------------

impl Config {
    /// `%APPDATA%\recall-hdr\config.toml`, creating the directory if missing.
    /// Falls back to the temp dir if `APPDATA` is unset (parity with `logging`).
    pub fn path() -> PathBuf {
        let base = crate::user_dirs::appdata().unwrap_or_else(std::env::temp_dir);
        let path = crate::user_dirs::checked(base.join("recall-hdr").join("config.toml"));
        if let Some(dir) = path.parent() {
            let _ = fs::create_dir_all(dir);
        }
        path
    }

    /// Load the config, or fall back to [`Config::default`] on any error.
    ///
    /// Never panics. A malformed file is backed up to a dated `config.toml.bad-*` (so the
    /// user can inspect/repair it) and defaults are returned. A missing file is
    /// the normal first-run path and returns defaults silently.
    pub fn load_or_default() -> Config {
        let path = Self::path();
        load_from(&path)
    }

    /// Serialize + atomically persist to [`Config::path`], then mirror the
    /// autostart flag into the registry (best-effort). This is the legacy
    /// (`--legacy-hdr`) tray's save path, so its Run value relaunches that mode.
    pub fn save(&self) -> Result<(), String> {
        self.save_to(&Self::path())?;
        // Mirror autostart. Best-effort: a registry failure must not fail a save
        // that already succeeded on disk.
        if let Err(e) = sync_autostart(self.startup.autostart, LEGACY_RUN_ARG) {
            log!("config: autostart sync failed (non-fatal): {e}");
        }
        Ok(())
    }

    /// Serialize + atomically persist to `path` with NO registry side effect
    /// (the unified record mode syncs the Run key only when autostart changes).
    ///
    /// Atomicity: we write to `<path>.tmp` and rename over the target, so a
    /// crash mid-write can never truncate the live config.
    pub fn save_to(&self, path: &Path) -> Result<(), String> {
        let toml_str = io::serialize_for(self, path)?;
        crate::durable::write_durable(path, toml_str.as_bytes())?;
        // The last good config is the last one saved, not the one read at start.
        io::remember_good(path, toml_str.as_bytes());
        Ok(())
    }

    /// Resolve the (rule, clamp) pair for a foreground exe stem.
    ///
    /// Override lookup is case-insensitive on the stem. A matched override's
    /// `clamp` wins if present, else `default_clamp`. No match -> the behavior
    /// defaults (`default_game_class`, `default_clamp`).
    pub fn rule_for(&self, exe_stem: &str) -> (RuleKind, ClampMode) {
        for o in &self.overrides {
            if crate::rules::same_exe(&o.exe, exe_stem) {
                return (o.kind, o.clamp.unwrap_or(self.behavior.default_clamp));
            }
        }
        (
            self.behavior.default_game_class,
            self.behavior.default_clamp,
        )
    }
}

/// Load a config from an explicit path (testable without touching `%APPDATA%`).
/// A file that exists but can't be read gives the defaults (logged); a caller that
/// saves uses [`load_checked`] instead, so it never saves the defaults over it.
pub(crate) fn load_from(path: &std::path::Path) -> Config {
    load_checked(path).unwrap_or_else(|e| {
        log!("config: read {} failed: {e}; using defaults", path.display());
        Config::default()
    })
}

/// [`load_from`], with Err when the file exists but could not be read (retried
/// briefly first: a backup tool or a sync client can hold it for a moment).
pub(crate) fn load_checked(path: &std::path::Path) -> Result<Config, String> {
    let now = std::time::SystemTime::now();
    crate::durable::sweep_temps(path, now);
    // Missing file is the normal first-run case -- don't shout about it.
    let Some(bytes) = io::read_retrying(path, &std::thread::sleep).map_err(|e| e.to_string())? else {
        let mut cfg = Config::default();
        cfg.recording.audio_sources = crate::record_audio_sources::initialize_sources();
        return Ok(cfg);
    };
    Ok(load_bytes(path, &bytes, now))
}

/// The config in `bytes`, already read from `path`: parsed, or salvaged per key, or the
/// last good load, else the defaults; migrated and repaired (`config_validate`). Also used
/// by `config_watch` on the text it read, so a hand edit is never read twice.
pub(crate) fn load_bytes(path: &std::path::Path, bytes: &[u8], now: std::time::SystemTime) -> Config {
    let Some((mut cfg, text, mut repairs)) = io::parse_or_recover(path, bytes, now) else {
        return Config::default();
    };
    // Legacy single `save_hotkey` -> `save_hotkeys` presets; a file from
    // the two-step-shortcut era gets the direct defaults.
    crate::record_hotkeys::migrate_loaded(&mut cfg.recording, &text);
    // Old audio keys -> `audio_sources`.
    crate::record_audio_sources::migrate_loaded(&mut cfg.recording, &text);
    // Pre-v2 corners were never honoured -> `dynamic`.
    cfg.overlay.migrate_loaded(&text);
    // The dev test card the defaults once shipped (F-detect-6): gone from installed builds.
    if !cfg!(debug_assertions) && drop_shipped_testcard(&mut cfg.overrides) {
        log!("config: removed the hdr-testcard entry older builds shipped (a dev test target)");
    }
    // Out-of-range values clamp, bad entries go (one log line each), never the whole file.
    // Runs on every parsed load: full parse, per-key salvage and last-good.
    repairs.extend(crate::config_validate::sanitize_loaded(&mut cfg));
    crate::config_repairs::remember(path, repairs);
    cfg
}

/// Drop the `hdr-testcard` override exactly as older defaults shipped it (`hdr_game`, no
/// clamp); an entry the user changed stays. `true` = one was dropped.
pub(crate) fn drop_shipped_testcard(list: &mut Vec<Override>) -> bool {
    let before = list.len();
    list.retain(|o| !(o.exe == "hdr-testcard" && o.kind == RuleKind::HdrGame && o.clamp.is_none()));
    list.len() != before
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
#[path = "config_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "config_persist_tests.rs"]
mod persist_tests;
