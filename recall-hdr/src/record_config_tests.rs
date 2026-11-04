use super::*;
use crate::types::RuleKind;
use serde_json::json;

/// The user's real config shape (2026-10-22): autostart off, 360-nit SDR
/// white, a `brave -> none` override, comments, and no `[recording]` yet.
const USER_TOML: &str = r#"
version = 1
[display]
target_friendly_name = "PG27AQDP"
[behavior]
default_game_class = "sdr_game"
default_clamp = "punchy"
# SDR-content white level comment
sdr_white_nits = 360
debounce_ms = 400
[hotkey]
toggle_hdr = "Ctrl+Alt+H"
[startup]
autostart = false
[[overrides]]
exe = "hdr-testcard"
kind = "hdr_game"
[[overrides]]
exe = "brave"
kind = "none"
"#;

fn temp_store(tag: &str) -> ConfigStore {
    let dir = std::env::temp_dir().join(format!(
        "recall-hdr-reccfg-{tag}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    ConfigStore::at(dir.join("config.toml"))
}

fn user_cfg() -> Config {
    toml::from_str(USER_TOML).expect("user config parses")
}

#[test]
fn user_config_without_recording_sections_gets_defaults() {
    let cfg = user_cfg();
    assert_eq!(cfg.recording, RecordingCfg::default());
    assert_eq!(cfg.overlay, OverlayCfg::default());
    assert!(!cfg.behavior.hdr_auto, "hdr_auto defaults off");
    let videos = std::path::Path::new(&cfg.recording.output_dir);
    assert!(videos.ends_with("Recall") && videos.is_absolute(), "Videos\\Recall, got {videos:?}");
    assert_eq!(cfg.recording.buffer_secs, 120);
    assert_eq!(cfg.recording.cpu_mask, "auto", "no machine-specific CPUs by default");
}

#[test]
fn partial_recording_section_fills_missing_keys() {
    let src = format!("{USER_TOML}\n[recording]\nbuffer_secs = 90\n");
    // `[recording]` must precede `[[overrides]]` only for serializing; TOML
    // allows it anywhere on load.
    let cfg: Config = toml::from_str(&src).unwrap();
    assert_eq!(cfg.recording.buffer_secs, 90);
    assert_eq!(cfg.recording.save_hotkey, "Alt+N");
    assert!(cfg.recording.auto_record);
}

#[test]
fn load_patch_save_reload_preserves_user_values() {
    let store = temp_store("roundtrip");
    std::fs::write(store.path(), USER_TOML).unwrap();
    let loaded = store.load();
    let out = apply_patch(
        &loaded,
        &json!({"overlay":{"enabled":true,"corner":"bottom-left"},"recording":{"buffer_secs":90}}),
    )
    .unwrap();
    store.save(&out.config).unwrap();
    let back = store.load();
    // The patched values landed...
    assert!(back.overlay.enabled);
    assert_eq!(back.overlay.corner, "bottom-left");
    assert_eq!(back.recording.buffer_secs, 90);
    // ...and every user value survived the round trip.
    assert!(!back.startup.autostart);
    assert_eq!(back.behavior.sdr_white_nits, 360);
    assert_eq!(back.overrides.len(), 2);
    assert_eq!(back.overrides[1].exe, "brave");
    assert_eq!(back.overrides[1].kind, RuleKind::None);
    assert_eq!(back, out.config, "save -> load is identity");
    let _ = std::fs::remove_dir_all(store.path().parent().unwrap());
}

#[test]
fn patch_reports_only_changed_leaves() {
    let base = user_cfg();
    let out = apply_patch(
        &base,
        &json!({"startup":{"autostart":true},"overlay":{"corner":"top-right"}}),
    )
    .unwrap();
    assert_eq!(out.changed, vec!["startup.autostart".to_string()], "corner was already top-right");
    assert!(out.touched("startup"));
    assert!(!out.touched("overlay"));
    assert!(!out.touched("start"), "prefix match is per path segment");
}

#[test]
fn patch_rejects_unknown_keys_bad_values_and_version() {
    let base = user_cfg();
    for (patch, needle) in [
        (json!({"recording":{"bufer_secs":90}}), "unknown config key 'recording.bufer_secs'"),
        (json!({"nope":1}), "unknown config key 'nope'"),
        (json!({"recording":5}), "must be an object"),
        (json!({"overlay":{"corner":"middle"}}), "overlay.corner"),
        (json!({"recording":{"buffer_secs":5}}), "buffer_secs"),
        (json!({"recording":{"cpu_mask":"0"}}), "cpu_mask"),
        (json!({"recording":{"cpu_mask":"zz"}}), "cpu_mask"),
        (json!({"recording":{"save_hotkey":"Ctrl+Nonsense"}}), "save_hotkey"),
        (json!({"recording":{"save_hotkey":"Alt+M"}}), "must differ"),
        (json!({"recording":{"buffer_secs":"ninety"}}), "invalid config"),
        (json!({"version":2}), "read-only"),
        (json!([1, 2]), "must be an object"),
    ] {
        let err = apply_patch(&base, &patch).expect_err(&format!("{patch} must be rejected"));
        assert!(err.contains(needle), "{patch}: '{err}' lacks '{needle}'");
    }
}

#[test]
fn patch_replaces_overrides_array_and_allows_optional_device_path() {
    let base = user_cfg();
    let out = apply_patch(
        &base,
        &json!({"overrides":[{"exe":"valorant","kind":"sdr_game"}],"display":{"target_device_path":"\\\\?\\DISPLAY#X"}}),
    )
    .unwrap();
    assert_eq!(out.config.overrides.len(), 1);
    assert_eq!(out.config.overrides[0].exe, "valorant");
    assert!(out.config.display.target_device_path.is_some());
    assert!(out.touched("overrides"));
}

#[test]
fn merge_json_merges_objects_and_replaces_everything_else() {
    let mut base = json!({"a":{"x":1,"y":2},"list":[1,2],"s":"old"});
    merge_json(&mut base, &json!({"a":{"y":3},"list":[9],"s":"new"}));
    assert_eq!(base, json!({"a":{"x":1,"y":3},"list":[9],"s":"new"}));
}

#[test]
fn cpu_mask_parses_with_or_without_prefix() {
    assert_eq!(parse_cpu_mask("ffff0000"), Ok(0xffff_0000));
    assert_eq!(parse_cpu_mask("0xFFFF0000"), Ok(0xffff_0000));
    assert!(parse_cpu_mask("0").is_err());
    assert_eq!(parse_cpu_mask(" Auto "), Ok(crate::record_config::AUTO_CPU_MASK));
    assert!(parse_cpu_mask("").is_err());
}

#[test]
fn config_json_has_every_section() {
    let v = config_json(&user_cfg());
    for key in ["display", "behavior", "hotkey", "startup", "recording", "overlay", "storage", "hotkeys", "overrides"] {
        assert!(v.get(key).is_some(), "config event lacks '{key}'");
    }
    assert_eq!(v["behavior"]["default_clamp"], "punchy", "snake_case tokens on the wire");
    assert_eq!(v["recording"]["save_hotkey"], "Alt+N");
    assert_eq!(v["recording"]["save_hotkeys"], json!([{"hotkey":"Alt+N","seconds":0}]));
    assert_eq!(v["storage"], json!({"cap_gb": 0}));
    assert_eq!(
        v["hotkeys"],
        json!({"advanced_enabled": true, "leader": "Alt+Backquote", "save_seconds": 30, "window_ms": 1500})
    );
}

#[test]
fn defaults_for_the_hotkey_features_and_quality() {
    let rec = RecordingCfg::default();
    // Ruling 2026-10-22: two direct combos (Alt+N, Alt+M); the rest is the chord.
    assert_eq!((rec.bookmark_hotkey.as_str(), rec.screenshot_hotkey.as_str(), rec.record_hotkey.as_str()), ("", "", ""));
    assert_eq!(HotkeysCfg::default().leader, "Alt+Backquote");
    assert_eq!((rec.codec.as_str(), rec.fps, rec.bitrate_mbps, rec.quality.as_str()), ("h264", 60, 25, "balanced"));
    assert_eq!(rec.voice_app, "", "voice track off by default");
    assert_eq!((rec.mic_device.as_str(), rec.mic_enabled, rec.desktop_audio_device.as_str()), ("", true, ""), "Windows default devices, mic on");
    assert_eq!(StorageCfg::default().cap_gb, 0, "auto-clean off by default");
    assert!(validate(&Config::default()).is_ok(), "defaults never collide (incl. Alt+M)");
    assert_eq!(rec.save_seconds(0), 120, "0 = full buffer");
    assert_eq!(rec.save_seconds(30), 30);
    assert_eq!(rec.save_seconds(900), 120, "never more than the buffer");
}

#[test]
fn every_new_key_patches_saves_and_reloads() {
    let store = temp_store("newkeys");
    std::fs::write(store.path(), USER_TOML).unwrap();
    let patch = json!({
        "recording": {
            "bookmark_hotkey": "Ctrl+F7", "screenshot_hotkey": "", "record_hotkey": "Ctrl+F8",
            "codec": "hevc", "fps": 120, "bitrate_mbps": 50, "quality": "quality", "voice_app": "Discord.exe",
            "mic_device": "Microphone (3- RODECaster Duo Chat)", "mic_enabled": false, "desktop_audio_device": "Game (RØDECaster)",
            "save_hotkeys": [{"hotkey": "Alt+N", "seconds": 0}, {"hotkey": "Alt+J", "seconds": 15}, {"hotkey": "Alt+L", "seconds": 60}]
        },
        "storage": {"cap_gb": 500},
        "hotkeys": {"advanced_enabled": false, "leader": "Ctrl+F12", "save_seconds": 45, "window_ms": 2000}
    });
    let mut base = store.load();
    base.recording.screenshot_hotkey = "Ctrl+F9".into(); // so clearing it is a change
    let out = apply_patch(&base, &patch).unwrap();
    for key in [
        "recording.bookmark_hotkey", "recording.screenshot_hotkey", "recording.record_hotkey", "recording.codec",
        "recording.fps", "recording.bitrate_mbps", "recording.quality", "recording.voice_app",
        "recording.mic_device", "recording.mic_enabled", "recording.desktop_audio_device",
        "recording.save_hotkeys", "storage.cap_gb", "hotkeys.advanced_enabled", "hotkeys.leader",
        "hotkeys.save_seconds", "hotkeys.window_ms",
    ] {
        assert!(out.touched(key), "{key} not reported as changed: {:?}", out.changed);
    }
    store.save(&out.config).unwrap();
    let text = std::fs::read_to_string(store.path()).unwrap();
    assert!(text.contains("[[recording.save_hotkeys]]"), "presets are TOML tables:\n{text}");
    let back = store.load();
    assert_eq!(back, out.config, "save -> load is identity");
    assert_eq!(back.recording.save_hotkeys[2], SaveHotkey { hotkey: "Alt+L".into(), seconds: 60 });
    assert_eq!(back.recording.screenshot_hotkey, "", "empty = off survives");
    assert_eq!(back.storage.cap_gb, 500);
    assert_eq!(back.recording.mic_device, "Microphone (3- RODECaster Duo Chat)", "non-ASCII device names survive TOML");
    assert_eq!(back.recording.desktop_audio_device, "Game (RØDECaster)");
    assert_eq!(back.hotkeys, HotkeysCfg { advanced_enabled: false, leader: "Ctrl+F12".into(), save_seconds: 45, window_ms: 2000 });
    let _ = std::fs::remove_dir_all(store.path().parent().unwrap());
}

#[test]
fn new_keys_reject_bad_values() {
    let base = Config::default();
    let nine: Vec<Value> = (1..=9).map(|i| json!({"hotkey": format!("Ctrl+F{i}"), "seconds": i})).collect();
    for (patch, needle) in [
        (json!({"recording":{"codec":"vp9"}}), "recording.codec"),
        (json!({"recording":{"fps":5}}), "recording.fps"),
        (json!({"recording":{"bitrate_mbps":0}}), "recording.bitrate_mbps"),
        (json!({"recording":{"quality":"max"}}), "recording.quality"),
        (json!({"recording":{"voice_app":"C:\\Apps\\Discord.exe"}}), "recording.voice_app"),
        (json!({"recording":{"mic_device":"Mic\u{7}Two"}}), "recording.mic_device"),
        (json!({"recording":{"desktop_audio_device":"x".repeat(300)}}), "recording.desktop_audio_device"),
        (json!({"recording":{"save_hotkeys": nine}}), "at most 8"),
        (json!({"recording":{"save_hotkeys":[{"hotkey":"Alt+N","seconds":9999}]}}), "seconds"),
        (json!({"recording":{"save_hotkeys":[{"hotkey":"","seconds":0}]}}), "must not be empty"),
        (json!({"recording":{"save_hotkeys":[{"hotkey":"Alt+Q","seconds":0},{"hotkey":"alt+q","seconds":5}]}}), "must differ"),
        (json!({"recording":{"bookmark_hotkey":"Alt+N"}}), "must differ"),
        (json!({"recording":{"screenshot_hotkey":"Alt+M"}}), "must differ"),
        (json!({"recording":{"record_hotkey":"Ctrl+Nope"}}), "recording.record_hotkey"),
        (json!({"recording":{"mic_hotkey":""}}), "mic_hotkey"),
        (json!({"storage":{"cap_gb":-1}}), "invalid config"),
        (json!({"storage":{"cap_gb":1}}), "storage.cap_gb must be 0 (off) or at least 20 GiB, got 1"),
        (json!({"storage":{"cap_gb":19}}), "at least 20 GiB"),
        (json!({"storage":{"cap":5}}), "unknown config key 'storage.cap'"),
        (json!({"hotkeys":{"leader":"Alt+N"}}), "must differ"),
        (json!({"hotkeys":{"leader":""}}), "hotkeys.leader"),
        (json!({"hotkeys":{"save_seconds":0}}), "hotkeys.save_seconds"),
        (json!({"hotkeys":{"window_ms":60000}}), "hotkeys.window_ms"),
        (json!({"hotkeys":{"keys":"1234"}}), "unknown config key 'hotkeys.keys'"),
    ] {
        let err = apply_patch(&base, &patch).expect_err(&format!("{patch} must be rejected"));
        assert!(err.contains(needle), "{patch}: '{err}' lacks '{needle}'");
    }
}

#[test]
fn legacy_save_hotkey_patch_moves_the_full_buffer_preset() {
    let base = Config::default();
    let out = apply_patch(&base, &json!({"recording":{"save_hotkey":"Ctrl+F9"}})).unwrap();
    assert_eq!(out.config.recording.save_hotkeys, vec![SaveHotkey { hotkey: "Ctrl+F9".into(), seconds: 0 }]);
    assert_eq!(out.config.recording.save_hotkey, "Ctrl+F9");
    // Extra presets survive a legacy move.
    let two = apply_patch(&base, &json!({"recording":{"save_hotkeys":[{"hotkey":"Alt+N","seconds":0},{"hotkey":"Ctrl+F5","seconds":30}]}})).unwrap().config;
    let out = apply_patch(&two, &json!({"recording":{"save_hotkey":"Ctrl+F9"}})).unwrap();
    assert_eq!(out.config.recording.save_hotkeys[1].hotkey, "Ctrl+F5", "the short preset stays");
    // A presets patch re-derives the legacy mirror...
    let out = apply_patch(&base, &json!({"recording":{"save_hotkeys":[{"hotkey":"Alt+J","seconds":20},{"hotkey":"F10","seconds":0}]}})).unwrap();
    assert_eq!(out.config.recording.save_hotkey, "F10", "first full-buffer preset");
    // ...and wins over a legacy key in the same patch.
    let out = apply_patch(&base, &json!({"recording":{"save_hotkey":"F11","save_hotkeys":[{"hotkey":"F12","seconds":0}]}})).unwrap();
    assert_eq!(out.config.recording.save_hotkey, "F12");
    // No full-buffer preset: the legacy key inserts one first.
    let short_only = apply_patch(&base, &json!({"recording":{"save_hotkeys":[{"hotkey":"Ctrl+F5","seconds":30}]}})).unwrap().config;
    let out = apply_patch(&short_only, &json!({"recording":{"save_hotkey":"Alt+N"}})).unwrap();
    assert_eq!(
        out.config.recording.save_hotkeys,
        vec![SaveHotkey { hotkey: "Alt+N".into(), seconds: 0 }, SaveHotkey { hotkey: "Ctrl+F5".into(), seconds: 30 }]
    );
}

#[test]
fn config_files_without_presets_migrate_from_save_hotkey() {
    let store = temp_store("migrate");
    let write_load = |extra: &str| {
        std::fs::write(store.path(), format!("{USER_TOML}\n[recording]\n{extra}\n")).unwrap();
        store.load().recording
    };
    let rec = write_load(r#"save_hotkey = "F9""#);
    assert_eq!(rec.save_hotkeys, vec![SaveHotkey { hotkey: "F9".into(), seconds: 0 }], "the legacy key keeps saving the full buffer");
    assert_eq!(rec.save_hotkey, "F9");
    let rec = write_load("save_hotkey = \"F9\"\n[[recording.save_hotkeys]]\nhotkey = \"F2\"\nseconds = 10\n");
    assert_eq!(rec.save_hotkeys, vec![SaveHotkey { hotkey: "F2".into(), seconds: 10 }], "an explicit list is kept as written");
    assert_eq!(rec.save_hotkey, "F2", "mirror follows the list");
    std::fs::write(store.path(), USER_TOML).unwrap();
    assert_eq!(store.load().recording, RecordingCfg::default(), "no [recording] at all -> defaults");
    let _ = std::fs::remove_dir_all(store.path().parent().unwrap());
}

#[test]
fn storage_cap_is_off_or_at_least_the_minimum() {
    let base = Config::default();
    for ok in [0, STORAGE_CAP_MIN_GB, 500] {
        let out = apply_patch(&base, &json!({"storage":{"cap_gb": ok}})).unwrap_or_else(|e| panic!("cap {ok}: {e}"));
        assert_eq!(out.config.storage.cap_gb, ok);
    }
    assert!(apply_patch(&base, &json!({"storage":{"cap_gb": STORAGE_CAP_MIN_GB - 1}})).is_err());
}

#[test]
fn overlay_stats_default_off_minimal_and_validated() {
    let cfg = user_cfg();
    assert!(!cfg.overlay.stats, "the stats island is hidden by default");
    assert_eq!(cfg.overlay.stats_style, "minimal");
    let v = config_json(&cfg);
    assert_eq!(v["overlay"]["stats"], false, "the app reads overlay.stats from the config event");
    assert_eq!(v["overlay"]["stats_style"], "minimal");
    let err = apply_patch(&cfg, &json!({"overlay":{"stats_style":"speedometer"}})).unwrap_err();
    assert!(err.contains("overlay.stats_style 'speedometer' must be one of minimal"), "{err}");
    assert!(apply_patch(&cfg, &json!({"overlay":{"stats":"yes"}})).is_err(), "a bool, not text");
}

#[test]
fn stats_toggle_patch_flips_persists_and_reports_the_key() {
    let store = temp_store("stats");
    std::fs::write(store.path(), USER_TOML).unwrap();
    let loaded = store.load();
    let on = apply_patch(&loaded, &crate::record_cmds::stats_patch(!loaded.overlay.stats)).unwrap();
    assert!(on.config.overlay.stats);
    assert_eq!(on.changed, vec!["overlay.stats".to_string()]);
    assert!(on.touched("overlay"));
    store.save(&on.config).unwrap();
    let back = store.load();
    assert!(back.overlay.stats, "survives a restart");
    assert!(!back.overlay.enabled, "notices untouched");
    let off = apply_patch(&back, &crate::record_cmds::stats_patch(!back.overlay.stats)).unwrap();
    assert!(!off.config.overlay.stats, "toggles back");
    // set_config of overlay.stats from the app is the same patch shape.
    let app = apply_patch(&back, &json!({"overlay":{"stats":false}})).unwrap();
    assert_eq!(app.config, off.config);
    let _ = std::fs::remove_dir_all(store.path().parent().unwrap());
}

#[test]
fn cpu_mask_fits_the_machine() {
    use crate::record_config::fit_cpu_mask;
    assert_eq!(fit_cpu_mask(0xffff_0000, 0xffff_ffff), Some(0xffff_0000));
    assert_eq!(fit_cpu_mask(0xffff_0000, 0xffff), Some(0xff00), "16 threads: high half");
    assert_eq!(fit_cpu_mask(0xffff_0000, 0xff), Some(0xf0), "8 threads: high half");
    assert_eq!(fit_cpu_mask(0xffff_0000, 0xf), None, "4 threads: no pin");
    let auto = crate::record_config::AUTO_CPU_MASK;
    assert_eq!(fit_cpu_mask(auto, 0xffff_ffff), Some(0xffff_0000), "auto, 32 threads: high half");
    assert_eq!(fit_cpu_mask(auto, 0xfff), Some(0xfc0), "auto, 12 threads: high half");
}
