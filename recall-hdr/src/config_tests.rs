use super::*;
use super::autostart::{run_value, wide_nul, RUN_SUBKEY, RUN_VALUE_NAME};
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
    assert_eq!(c.display.target_friendly_name, "", "empty = the primary display");
    assert_eq!(c.display.target_device_path, None);
    assert_eq!(c.behavior.default_game_class, RuleKind::SdrGame);
    assert_eq!(c.behavior.default_clamp, ClampMode::Punchy);
    assert_eq!(c.behavior.sdr_white_nits, 220);
    assert_eq!(c.behavior.debounce_ms, 400);
    assert_eq!(c.hotkey.toggle_hdr, "Ctrl+Alt+H");
    assert!(!c.startup.autostart);
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

    let bad = crate::durable::bad_path(&path, std::time::SystemTime::now());
    assert!(bad.exists(), "malformed config must be preserved as a dated .bad");
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
    let mut cfg = load_from(&path);
    let fresh = std::mem::take(&mut cfg.recording.audio_sources);
    assert_eq!(fresh.iter().take(2).map(|s| s.id.as_str()).collect::<Vec<_>>(), ["game", "mic"], "first run: the game and the default mic");
    cfg.recording.audio_sources = crate::record_audio_sources::default_sources();
    assert_eq!(cfg, Config::default());
    // No .bad file for a simple missing config.
    assert!(!crate::durable::bad_path(&path, std::time::SystemTime::now()).exists());
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
