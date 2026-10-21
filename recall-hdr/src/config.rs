//! Config schema + TOML persistence (`%APPDATA%\recall-hdr\config.toml`) and the
//! `HKCU\...\Run` autostart mirror.
//!
//! The schema is design-frozen (Wave 1). It mirrors the frozen `types.rs`
//! contracts (`RuleKind`, `ClampMode`) rather than redefining them, so a config
//! `default_clamp = "punchy"` deserializes straight into the same [`ClampMode`]
//! the reconciler consumes.
//!
//! Persistence rules, all "never lose the user's display in a bad state":
//! - [`Config::load_or_default`] never panics: a malformed TOML is backed up to
//!   `config.toml.bad` and we fall back to [`Config::default`].
//! - [`Config::save`] is atomic: write `config.toml.tmp`, then rename over the
//!   real file, so a crash mid-write can't leave a truncated config.
//! - [`Config::save`] also mirrors `startup.autostart` into the `HKCU` Run key
//!   (best-effort -- a registry failure is logged, not fatal to the save).

use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::log;
use crate::record_config::{OverlayCfg, RecordingCfg};
use crate::types::{ClampMode, RuleKind};

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
    /// Per-exe rule overrides. Exe stems are matched case-insensitively.
    #[serde(default, rename = "overrides")]
    pub overrides: Vec<Override>,
}

/// `[display]` -- which panel we manage.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DisplayCfg {
    /// Friendly name (e.g. `"PG27AQDP"`) used to select the target display.
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

impl Default for DisplayCfg {
    fn default() -> Self {
        DisplayCfg {
            target_friendly_name: "PG27AQDP".into(),
            target_device_path: None,
        }
    }
}

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
        StartupCfg { autostart: true }
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
            // The hdr-testcard entry is our live test target; ship it by default.
            overrides: vec![Override {
                exe: "hdr-testcard".into(),
                kind: RuleKind::HdrGame,
                clamp: None,
            }],
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
        let base = std::env::var("APPDATA")
            .map(PathBuf::from)
            .unwrap_or_else(|_| std::env::temp_dir());
        let dir = base.join("recall-hdr");
        let _ = fs::create_dir_all(&dir);
        dir.join("config.toml")
    }

    /// Load the config, or fall back to [`Config::default`] on any error.
    ///
    /// Never panics. A malformed file is backed up to `config.toml.bad` (so the
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
        let toml_str =
            toml::to_string_pretty(self).map_err(|e| format!("serialize config: {e}"))?;
        if let Some(dir) = path.parent() {
            let _ = fs::create_dir_all(dir);
        }
        let tmp = path.with_extension("toml.tmp");
        fs::write(&tmp, toml_str.as_bytes())
            .map_err(|e| format!("write {}: {e}", tmp.display()))?;
        // rename is atomic on the same volume; on Windows it replaces the target.
        fs::rename(&tmp, path).map_err(|e| {
            // Best-effort cleanup so a failed rename doesn't strand the .tmp.
            let _ = fs::remove_file(&tmp);
            format!("rename {} -> {}: {e}", tmp.display(), path.display())
        })
    }

    /// Resolve the (rule, clamp) pair for a foreground exe stem.
    ///
    /// Override lookup is case-insensitive on the stem. A matched override's
    /// `clamp` wins if present, else `default_clamp`. No match -> the behavior
    /// defaults (`default_game_class`, `default_clamp`).
    pub fn rule_for(&self, exe_stem: &str) -> (RuleKind, ClampMode) {
        let needle = exe_stem.to_ascii_lowercase();
        for o in &self.overrides {
            if o.exe.eq_ignore_ascii_case(&needle) {
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
pub(crate) fn load_from(path: &std::path::Path) -> Config {
    let text = match fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) => {
            // Missing file is the normal first-run case -- don't shout about it.
            if e.kind() != std::io::ErrorKind::NotFound {
                log!("config: read {} failed: {e}; using defaults", path.display());
            }
            return Config::default();
        }
    };

    match toml::from_str::<Config>(&text) {
        Ok(cfg) => cfg,
        Err(e) => {
            log!("config: parse {} failed: {e}; backing up + using defaults", path.display());
            let bad = path.with_extension("toml.bad");
            if let Err(be) = fs::rename(path, &bad) {
                // If rename fails (e.g. cross-device), fall back to a copy so the
                // bad file is preserved and the next save can overwrite the orig.
                if let Err(ce) = fs::copy(path, &bad) {
                    log!("config: could not back up bad config (rename: {be}, copy: {ce})");
                }
            }
            Config::default()
        }
    }
}

// ---------------------------------------------------------------------------
// Autostart mirror (HKCU Run key)
// ---------------------------------------------------------------------------

/// Registry subkey holding per-user run-at-login entries.
const RUN_SUBKEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
/// The value name we own under [`RUN_SUBKEY`].
const RUN_VALUE_NAME: &str = "recall-hdr";

/// Run-key argument that relaunches the unified record mode at login.
pub const RECORD_RUN_ARG: &str = "--record";
/// Run-key argument that relaunches the legacy HDR-only tray at login.
pub const LEGACY_RUN_ARG: &str = "--legacy-hdr";

/// The exact `REG_SZ` data written under the Run key: the quoted exe path plus
/// the mode argument, e.g. `"C:\Recall\recall-hdr.exe" --record`. Pure so the
/// string is unit-tested without touching the registry.
pub fn run_value(exe: &str, arg: &str) -> String {
    if arg.is_empty() {
        format!("\"{exe}\"")
    } else {
        format!("\"{exe}\" {arg}")
    }
}

/// Mirror an autostart flag into `HKCU\...\Run`.
///
/// Idempotent:
/// - `enabled == true`  -> set `recall-hdr` = [`run_value`]`(current_exe, arg)`.
/// - `enabled == false` -> delete the value if present (absent is success).
///
/// Uses the `windows-rs` `Win32::System::Registry` surface
/// (`RegCreateKeyW` / `RegSetValueExW` / `RegDeleteValueW`). Errors are
/// returned as strings so the caller can log-and-continue.
pub fn sync_autostart(enabled: bool, arg: &str) -> Result<(), String> {
    if enabled {
        let exe = std::env::current_exe()
            .map_err(|e| format!("current_exe: {e}"))?
            .to_string_lossy()
            .into_owned();
        set_run_value(&run_value(&exe, arg))
    } else {
        delete_run_value()
    }
}

/// Encode a Rust string as a NUL-terminated UTF-16 buffer for the W registry API.
fn wide_nul(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// Create-or-open the Run key for write and set our value to `data` (REG_SZ).
///
/// Uses `RegCreateKeyW` (create-or-open, default security, all-access) rather
/// than `RegCreateKeyExW`: the Ex variant takes a `SECURITY_ATTRIBUTES` pointer
/// and is gated behind the `Win32_Security` cargo feature, which this crate's
/// pinned feature set does not enable. The `...\Run` key exists on every Windows
/// install, so this is effectively an open-for-write.
fn set_run_value(data: &str) -> Result<(), String> {
    use windows::core::PCWSTR;
    use windows::Win32::System::Registry::{
        RegCloseKey, RegCreateKeyW, RegSetValueExW, HKEY, HKEY_CURRENT_USER, REG_SZ,
    };

    let sub_w = wide_nul(RUN_SUBKEY);
    let name_w = wide_nul(RUN_VALUE_NAME);
    // REG_SZ payload is the UTF-16 string *including* its terminating NUL, as
    // raw bytes (len = u16 count * 2).
    let data_w = wide_nul(data);
    let data_bytes: &[u8] = unsafe {
        std::slice::from_raw_parts(data_w.as_ptr() as *const u8, data_w.len() * 2)
    };

    unsafe {
        let mut key = HKEY::default();
        let st = RegCreateKeyW(HKEY_CURRENT_USER, PCWSTR(sub_w.as_ptr()), &mut key);
        if st.is_err() {
            return Err(format!("RegCreateKeyW(Run) failed: {st:?}"));
        }

        let set = RegSetValueExW(
            key,
            PCWSTR(name_w.as_ptr()),
            Some(0),
            REG_SZ,
            Some(data_bytes),
        );
        let _ = RegCloseKey(key);

        if set.is_err() {
            return Err(format!("RegSetValueExW({RUN_VALUE_NAME}) failed: {set:?}"));
        }
    }
    Ok(())
}

/// Delete our Run value. A missing value (ERROR_FILE_NOT_FOUND) is success.
fn delete_run_value() -> Result<(), String> {
    use windows::core::PCWSTR;
    use windows::Win32::Foundation::ERROR_FILE_NOT_FOUND;
    use windows::Win32::System::Registry::{
        RegCloseKey, RegDeleteValueW, RegOpenKeyExW, HKEY, HKEY_CURRENT_USER, KEY_WRITE,
    };

    let sub_w = wide_nul(RUN_SUBKEY);
    let name_w = wide_nul(RUN_VALUE_NAME);

    unsafe {
        let mut key = HKEY::default();
        let open = RegOpenKeyExW(
            HKEY_CURRENT_USER,
            PCWSTR(sub_w.as_ptr()),
            Some(0),
            KEY_WRITE,
            &mut key,
        );
        if open.is_err() {
            // No Run key at all -> nothing to delete -> success.
            if open == ERROR_FILE_NOT_FOUND {
                return Ok(());
            }
            return Err(format!("RegOpenKeyExW(Run) failed: {open:?}"));
        }

        let del = RegDeleteValueW(key, PCWSTR(name_w.as_ptr()));
        let _ = RegCloseKey(key);

        if del.is_err() && del != ERROR_FILE_NOT_FOUND {
            return Err(format!("RegDeleteValueW({RUN_VALUE_NAME}) failed: {del:?}"));
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    /// A unique scratch dir under the OS temp dir (no `%APPDATA%` involvement).
    fn temp_dir(tag: &str) -> PathBuf {
        let mut p = std::env::temp_dir();
        let uniq = format!(
            "recall-hdr-test-{tag}-{}-{:?}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        );
        p.push(uniq);
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    #[test]
    fn default_roundtrips_through_toml() {
        let cfg = Config::default();
        let s = toml::to_string_pretty(&cfg).expect("serialize");
        let back: Config = toml::from_str(&s).expect("deserialize");
        assert_eq!(cfg, back, "default -> toml -> parse must be identity");
    }

    #[test]
    fn default_values_match_frozen_schema() {
        let c = Config::default();
        assert_eq!(c.version, 1);
        assert_eq!(c.display.target_friendly_name, "PG27AQDP");
        assert_eq!(c.display.target_device_path, None);
        assert_eq!(c.behavior.default_game_class, RuleKind::SdrGame);
        assert_eq!(c.behavior.default_clamp, ClampMode::Punchy);
        assert_eq!(c.behavior.sdr_white_nits, 220);
        assert_eq!(c.behavior.debounce_ms, 400);
        assert_eq!(c.hotkey.toggle_hdr, "Ctrl+Alt+H");
        assert!(c.startup.autostart);
        assert_eq!(c.overrides.len(), 1);
        assert_eq!(c.overrides[0].exe, "hdr-testcard");
        assert_eq!(c.overrides[0].kind, RuleKind::HdrGame);
        assert_eq!(c.overrides[0].clamp, None);
    }

    #[test]
    fn enum_tokens_match_frozen_design_snake_case() {
        // The design-frozen schema pins snake_case tokens. The `#[serde(with)]`
        // adapters bridge the frozen PascalCase enums to exactly these tokens, so
        // a user can hand-edit the config per the schema doc without a parse
        // error. Pin the wire surface so it can't silently drift.
        let s = toml::to_string_pretty(&Config::default()).unwrap();
        assert!(
            s.contains("default_game_class = \"sdr_game\""),
            "expected snake_case rule token; got:\n{s}"
        );
        assert!(
            s.contains("default_clamp = \"punchy\""),
            "expected snake_case clamp token; got:\n{s}"
        );
        assert!(
            s.contains("kind = \"hdr_game\""),
            "expected snake_case override kind token; got:\n{s}"
        );
    }

    #[test]
    fn snake_case_tokens_deserialize_back_to_frozen_enums() {
        // Round-trip through the design's exact tokens, including a per-exe clamp.
        let toml_src = r#"
[behavior]
default_game_class = "hdr_game"
default_clamp = "accurate"
sdr_white_nits = 210
debounce_ms = 300
[[overrides]]
exe = "x"
kind = "sdr_game"
clamp = "off"
"#;
        let cfg: Config = toml::from_str(toml_src).unwrap();
        assert_eq!(cfg.behavior.default_game_class, RuleKind::HdrGame);
        assert_eq!(cfg.behavior.default_clamp, ClampMode::Accurate);
        assert_eq!(cfg.overrides[0].kind, RuleKind::SdrGame);
        assert_eq!(cfg.overrides[0].clamp, Some(ClampMode::Off));
    }

    #[test]
    fn rule_for_override_hit_uses_override_kind() {
        let cfg = Config::default(); // has hdr-testcard -> HdrGame, clamp None
        let (kind, clamp) = cfg.rule_for("hdr-testcard");
        assert_eq!(kind, RuleKind::HdrGame);
        // clamp None on the override -> falls back to default_clamp (Punchy).
        assert_eq!(clamp, ClampMode::Punchy);
    }

    #[test]
    fn rule_for_override_is_case_insensitive() {
        let cfg = Config::default();
        let (kind, _) = cfg.rule_for("HDR-TestCard");
        assert_eq!(kind, RuleKind::HdrGame, "override match must ignore case");
    }

    #[test]
    fn rule_for_override_clamp_wins_when_present() {
        let mut cfg = Config::default();
        cfg.overrides.push(Override {
            exe: "witcher3".into(),
            kind: RuleKind::SdrGame,
            clamp: Some(ClampMode::Accurate),
        });
        let (kind, clamp) = cfg.rule_for("Witcher3");
        assert_eq!(kind, RuleKind::SdrGame);
        assert_eq!(clamp, ClampMode::Accurate, "per-exe clamp override wins");
    }

    #[test]
    fn rule_for_default_fallback_on_miss() {
        let cfg = Config::default();
        let (kind, clamp) = cfg.rule_for("some-unlisted-game");
        assert_eq!(kind, cfg.behavior.default_game_class);
        assert_eq!(clamp, cfg.behavior.default_clamp);
    }

    #[test]
    fn bad_toml_backs_up_and_returns_default() {
        let dir = temp_dir("badtoml");
        let path = dir.join("config.toml");
        std::fs::write(&path, b"this is = = not valid toml [[[").unwrap();

        let cfg = load_from(&path);
        assert_eq!(cfg, Config::default(), "malformed config -> defaults");

        let bad = path.with_extension("toml.bad");
        assert!(bad.exists(), "malformed config must be preserved as .bad");
        // Original should have been moved out of the way.
        assert!(!path.exists(), "bad config should be renamed, not left in place");

        // The .bad file preserves the original bytes.
        let preserved = std::fs::read_to_string(&bad).unwrap();
        assert!(preserved.contains("not valid toml"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn missing_file_returns_default_silently() {
        let dir = temp_dir("missing");
        let path = dir.join("does-not-exist.toml");
        let cfg = load_from(&path);
        assert_eq!(cfg, Config::default());
        // No .bad file for a simple missing config.
        assert!(!path.with_extension("toml.bad").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn valid_toml_parses_with_overridden_fields() {
        let dir = temp_dir("valid");
        let path = dir.join("config.toml");
        std::fs::write(
            &path,
            br#"
version = 1
[display]
target_friendly_name = "MyMonitor"
[behavior]
default_game_class = "none"
default_clamp = "accurate"
sdr_white_nits = 200
debounce_ms = 250
[hotkey]
toggle_hdr = "Ctrl+Shift+H"
[startup]
autostart = false
[[overrides]]
exe = "doom"
kind = "hdr_game"
clamp = "off"
"#,
        )
        .unwrap();

        let cfg = load_from(&path);
        assert_eq!(cfg.display.target_friendly_name, "MyMonitor");
        assert_eq!(cfg.behavior.default_game_class, RuleKind::None);
        assert_eq!(cfg.behavior.default_clamp, ClampMode::Accurate);
        assert_eq!(cfg.behavior.sdr_white_nits, 200);
        assert!(!cfg.startup.autostart);
        assert_eq!(cfg.overrides.len(), 1);
        assert_eq!(cfg.overrides[0].exe, "doom");
        assert_eq!(cfg.overrides[0].clamp, Some(ClampMode::Off));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Live registry test: writes the Run value, reads it back via a fresh
    /// process query, deletes it, and confirms deletion. Left `#[ignore]` so the
    /// default `cargo test` never mutates HKCU; run explicitly with `--ignored`.
    /// Leaves the registry exactly as found (value deleted at the end).
    #[test]
    #[ignore]
    fn autostart_write_readback_delete_live() {
        use windows::core::PCWSTR;
        use windows::Win32::System::Registry::{
            RegCloseKey, RegGetValueW, RegOpenKeyExW, HKEY, HKEY_CURRENT_USER, KEY_READ,
            RRF_RT_REG_SZ,
        };

        // Pre-state: was the value already present? (Should be no on a clean box,
        // but be honest about restoring exactly what we found.)
        let pre_existing = read_run_value_live();
        assert!(
            pre_existing.is_none(),
            "test refuses to run: a recall-hdr Run value already exists ({pre_existing:?}); \
             not clobbering it. Clean it manually if this is stale."
        );

        // 1. Enable -> value written.
        sync_autostart(true, RECORD_RUN_ARG).expect("sync_autostart(true) should write the Run value");

        // 2. Read it back and confirm it is our quoted current_exe + --record.
        let expected = run_value(
            &std::env::current_exe().unwrap().to_string_lossy(),
            RECORD_RUN_ARG,
        );
        let got = read_run_value_live().expect("Run value must exist after enable");
        assert_eq!(got, expected, "Run value must be the quoted current exe path");
        println!("autostart live: wrote + read back Run value = {got}");

        // 3. Idempotency: enabling again is a no-op that still leaves the value.
        sync_autostart(true, RECORD_RUN_ARG).expect("second enable should be idempotent");
        assert_eq!(read_run_value_live().as_deref(), Some(expected.as_str()));

        // 4. Disable -> value deleted.
        sync_autostart(false, RECORD_RUN_ARG).expect("sync_autostart(false) should delete the Run value");
        assert_eq!(read_run_value_live(), None, "Run value must be gone after disable");
        println!("autostart live: deleted Run value; registry left clean");

        // 5. Idempotency: disabling again when already absent is success.
        sync_autostart(false, RECORD_RUN_ARG).expect("second disable should be a no-op success");

        // Local reader used only by this live test.
        fn read_run_value_live() -> Option<String> {
            let sub_w = wide_nul(RUN_SUBKEY);
            let name_w = wide_nul(RUN_VALUE_NAME);
            unsafe {
                let mut key = HKEY::default();
                let open = RegOpenKeyExW(
                    HKEY_CURRENT_USER,
                    PCWSTR(sub_w.as_ptr()),
                    Some(0),
                    KEY_READ,
                    &mut key,
                );
                if open.is_err() {
                    return None;
                }
                let mut size: u32 = 0;
                let sz = RegGetValueW(
                    key,
                    PCWSTR::null(),
                    PCWSTR(name_w.as_ptr()),
                    RRF_RT_REG_SZ,
                    None,
                    None,
                    Some(&mut size),
                );
                if sz.is_err() || size == 0 {
                    let _ = RegCloseKey(key);
                    return None;
                }
                let mut buf = vec![0u8; size as usize];
                let rd = RegGetValueW(
                    key,
                    PCWSTR::null(),
                    PCWSTR(name_w.as_ptr()),
                    RRF_RT_REG_SZ,
                    None,
                    Some(buf.as_mut_ptr() as *mut _),
                    Some(&mut size),
                );
                let _ = RegCloseKey(key);
                if rd.is_err() {
                    return None;
                }
                // size is bytes including the trailing NUL; decode as UTF-16.
                let u16_len = (size as usize) / 2;
                let wide: Vec<u16> = (0..u16_len)
                    .map(|i| u16::from_le_bytes([buf[i * 2], buf[i * 2 + 1]]))
                    .collect();
                let end = wide.iter().position(|&c| c == 0).unwrap_or(wide.len());
                Some(String::from_utf16_lossy(&wide[..end]))
            }
        }
    }
}
