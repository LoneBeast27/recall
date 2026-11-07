use super::*;
use crate::record_config::{apply_patch, validate};
use serde_json::json;
use std::path::PathBuf;

fn temp_file(tag: &str, text: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "recall-hdr-validate-{tag}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("config.toml");
    std::fs::write(&path, text).unwrap();
    path
}

#[test]
fn loaded_fps_above_the_cap_clamps_and_later_patches_apply() {
    let path = temp_file("fps", "[recording]\noutput_dir = 'E:\\Clips'\nfps = 480\nbuffer_secs = 0\n");
    let cfg = crate::config::load_from(&path);
    assert_eq!(cfg.recording.fps, 240, "a loaded fps above 240 clamps to 240");
    assert_eq!(cfg.recording.buffer_secs, 10, "0 s clamps to the shortest buffer");
    assert_eq!(cfg.recording.output_dir, r"E:\Clips", "the rest of the file is kept");
    let out = apply_patch(&cfg, &json!({"recording": {"codec": "hevc"}}));
    assert!(out.is_ok(), "an unrelated patch must not fail on the loaded value: {out:?}");
}

#[test]
fn out_of_range_values_and_unknown_tokens_are_repaired_one_by_one() {
    let mut cfg = Config::default();
    cfg.recording.output_dir = r"E:\Clips".into();
    cfg.recording.bitrate_mbps = 0;
    cfg.recording.buffer_secs = 5000;
    cfg.recording.codec = "HEVC".into();
    cfg.recording.quality = "ultra".into();
    cfg.recording.cpu_mask = "fast".into();
    cfg.recording.voice_app = r"C:\chat.exe".into();
    cfg.recording.mic_device = "a\nb".into();
    cfg.overlay.corner = "Top-Right".into();
    cfg.overlay.stats_style = "fancy".into();
    cfg.storage.cap_gb = 15;
    cfg.midi.input = "x\u{7}".into();
    cfg.detect.fullscreen_games = vec!["Discord".into(), r"C:\a\b.exe".into(), " ".into()];
    let notes = sanitize_loaded(&mut cfg);
    let rec = &cfg.recording;
    assert_eq!((rec.bitrate_mbps, rec.buffer_secs), (1, 600));
    assert_eq!((rec.codec.as_str(), rec.quality.as_str(), rec.cpu_mask.as_str()), ("hevc", "balanced", "auto"));
    assert_eq!((rec.voice_app.as_str(), rec.mic_device.as_str()), ("", ""));
    assert_eq!(rec.output_dir, r"E:\Clips");
    assert_eq!((cfg.overlay.corner.as_str(), cfg.overlay.stats_style.as_str()), ("top-right", "minimal"));
    assert_eq!(cfg.storage.cap_gb, 20, "15 GiB is nearer the 20 GiB floor than off");
    assert_eq!(cfg.midi.input, "");
    assert_eq!(cfg.detect.fullscreen_games, vec!["Discord".to_string()], "only the bad entries go");
    assert!(notes.len() >= 12, "every repair is noted: {notes:?}");
    validate(&cfg).expect("a repaired config validates");
}

#[test]
fn a_small_cap_becomes_the_floor_never_off_and_empty_folder_gets_the_default() {
    for cap in [1, 3, 9, 10, 19] {
        let mut cfg = Config::default();
        cfg.storage.cap_gb = cap;
        cfg.recording.output_dir = "  ".into();
        sanitize_loaded(&mut cfg);
        assert_eq!(cfg.storage.cap_gb, 20, "a cap of {cap} GiB was meant as a cap: the 20 GiB floor, not off");
        assert_eq!(cfg.recording.output_dir, crate::record_config::default_output_dir());
    }
}

#[test]
fn bad_or_duplicate_hotkeys_are_cleared_not_the_whole_config() {
    let mut cfg = Config::default();
    let rec = &mut cfg.recording;
    rec.mic_hotkey = String::new();
    rec.bookmark_hotkey = "Alt+N".into(); // the save preset's combo
    rec.screenshot_hotkey = "Alt+Nope".into();
    rec.save_hotkeys.push(crate::record_config::SaveHotkey { hotkey: "Alt+N".into(), seconds: 30 });
    rec.save_hotkeys.push(crate::record_config::SaveHotkey { hotkey: "Alt+K".into(), seconds: 9000 });
    sanitize_loaded(&mut cfg);
    let rec = &cfg.recording;
    assert_eq!(rec.mic_hotkey, "Alt+M", "the mic shortcut gets its default");
    assert_eq!(rec.bookmark_hotkey, "", "a duplicate is turned off");
    assert_eq!(rec.screenshot_hotkey, "", "an unparseable one is turned off");
    let presets: Vec<(&str, u32)> = rec.save_hotkeys.iter().map(|s| (s.hotkey.as_str(), s.seconds)).collect();
    assert_eq!(presets, vec![("Alt+N", 0), ("Alt+K", 600)], "duplicate preset dropped, seconds clamped");
    assert_eq!(rec.save_hotkey, "Alt+N");
    validate(&cfg).expect("repaired hotkeys validate");
}

#[test]
fn a_duplicate_audio_source_drops_only_the_copy() {
    let path = temp_file(
        "audio",
        "[[recording.audio_sources]]\nid = 'game'\nkind = 'game'\nname = 'Game'\n\
         [[recording.audio_sources]]\nid = 'mic'\nkind = 'input'\nname = 'Microphone'\n\
         [[recording.audio_sources]]\nid = 'mic'\nkind = 'input'\nname = 'Second mic'\ndevice = 'USB'\n\
         [[recording.audio_sources]]\nid = 'line'\nkind = 'Input'\nname = 'Line in'\ngain_db = 20.0\n\
         [[recording.audio_sources]]\nid = 'spk'\nkind = 'speaker'\nname = 'Speaker'\n\
         [[recording.audio_sources]]\nid = 'app'\nkind = 'app'\nname = 'App'\n",
    );
    let cfg = crate::config::load_from(&path);
    let ids: Vec<&str> = cfg.recording.audio_sources.iter().map(|s| s.id.as_str()).collect();
    assert_eq!(ids, vec!["game", "mic", "line"], "copy, unknown kind and exe-less app dropped; Game kept");
    let line = &cfg.recording.audio_sources[2];
    assert_eq!((line.kind.as_str(), line.gain_db), ("input", 12.0), "kind spelling fixed, gain clamped");
    crate::record_audio_sources::validate(&cfg.recording.audio_sources).expect("the list validates");
    validate(&cfg).expect("the config validates");
}

#[test]
fn more_than_eight_sources_keep_the_first_eight() {
    let mut cfg = Config::default();
    for n in 0..9 {
        let mut s = cfg.recording.audio_sources[1].clone();
        s.id = format!("in-{n}");
        cfg.recording.audio_sources.push(s);
    }
    cfg.recording.audio_sources[0].gain_db = f32::NAN;
    sanitize_loaded(&mut cfg);
    assert_eq!(cfg.recording.audio_sources.len(), crate::record_audio_sources::SOURCES_MAX);
    assert_eq!(cfg.recording.audio_sources[0].gain_db, 0.0, "NaN gain becomes 0 dB");
    validate(&cfg).expect("validates");
}

#[test]
fn a_valid_config_is_left_alone() {
    let mut cfg = Config::default();
    let before = cfg.clone();
    assert!(sanitize_loaded(&mut cfg).is_empty());
    assert_eq!(cfg, before);
}

#[test]
fn a_mic_shortcut_on_a_save_preset_moves_and_the_save_stays() {
    let mut cfg = Config::default();
    cfg.recording.mic_hotkey = "Alt+N".into(); // the default save preset's combo
    sanitize_loaded(&mut cfg);
    let rec = &cfg.recording;
    assert_eq!(rec.save_hotkeys.first().map(|s| s.hotkey.as_str()), Some("Alt+N"), "the save preset keeps its key");
    assert_eq!(rec.mic_hotkey, "Alt+M", "the mic moves to its default");
    validate(&cfg).expect("validates");

    // Alt+M taken by a preset too: the first free fallback.
    let mut cfg = Config::default();
    cfg.recording.mic_hotkey = "Alt+N".into();
    cfg.recording.save_hotkeys.push(crate::record_config::SaveHotkey { hotkey: "Alt+M".into(), seconds: 30 });
    sanitize_loaded(&mut cfg);
    let presets: Vec<&str> = cfg.recording.save_hotkeys.iter().map(|s| s.hotkey.as_str()).collect();
    assert_eq!(presets, vec!["Alt+N", "Alt+M"]);
    assert_eq!(cfg.recording.mic_hotkey, "Ctrl+Alt+M");
    validate(&cfg).expect("validates");
    for f in crate::config_validate_lists::MIC_FALLBACKS {
        crate::hotkey::parse_accel(f).expect("every mic fallback parses");
    }
}

#[test]
fn a_long_non_ascii_device_name_is_cleared_by_bytes() {
    let mut cfg = Config::default();
    cfg.recording.mic_device = "\u{9ea6}".repeat(100); // 100 chars, 300 bytes
    cfg.recording.desktop_audio_device = "\u{9ea6}".repeat(80); // 240 bytes: kept
    sanitize_loaded(&mut cfg);
    assert_eq!(cfg.recording.mic_device, "", "validate counts bytes, so the repair does too");
    assert_eq!(cfg.recording.desktop_audio_device.chars().count(), 80);
    validate(&cfg).expect("a repaired config validates");
}

#[test]
fn a_loads_repairs_are_kept_for_the_controller_once() {
    let path = temp_file("notes", "[recording]\noutput_dir = 'E:\\Clips'\nfps = 480\n[storage]\ncap_gb = 5\n");
    let cfg = crate::config::load_from(&path);
    assert_eq!(cfg.recording.fps, 240);
    let notes = crate::config_repairs::take(&path);
    assert_eq!(notes.len(), 2, "fps and cap: {notes:?}");
    assert!(crate::config_repairs::take(&path).is_empty(), "taken once");
    let clean = temp_file("clean", "[recording]\noutput_dir = 'E:\\Clips'\n");
    crate::config::load_from(&clean);
    assert!(crate::config_repairs::take(&clean).is_empty());
}
