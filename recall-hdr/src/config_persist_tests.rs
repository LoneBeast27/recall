//! config.toml durability and corrupt-file handling (deep-audit S3, S6, S11).
use super::*;
use std::path::PathBuf;

fn temp_dir(tag: &str) -> PathBuf {
    let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
    let dir = std::env::temp_dir().join(format!("recall-hdr-persist-{tag}-{}-{nanos}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// Every `config.toml.bad*` file beside `path`, with its bytes.
fn backups(path: &Path) -> Vec<(String, Vec<u8>)> {
    let mut out: Vec<(String, Vec<u8>)> = std::fs::read_dir(path.parent().unwrap())
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.starts_with("config.toml.bad"))
        .map(|n| {
            let bytes = std::fs::read(path.with_file_name(&n)).unwrap();
            (n, bytes)
        })
        .collect();
    out.sort();
    out
}

#[test]
fn one_wrongly_typed_value_costs_only_that_key() {
    let dir = temp_dir("salvage");
    let path = dir.join("config.toml");
    let text = "[recording]\noutput_dir = 'E:\\Clips'\nfps = 59.94\nbuffer_secs = 300\n[storage]\ncap_gb = 50\n";
    std::fs::write(&path, text).unwrap();
    let cfg = load_from(&path);
    assert_eq!(cfg.recording.output_dir, r"E:\Clips", "the valid keys are kept");
    assert_eq!(cfg.recording.buffer_secs, 300);
    assert_eq!(cfg.storage.cap_gb, 50);
    assert_eq!(cfg.recording.fps, RecordingCfg::default().fps, "only the bad key is reset");
    let kept = backups(&path);
    assert_eq!(kept.len(), 1, "{kept:?}");
    assert_eq!(kept[0].1, text.as_bytes(), "the original bytes are kept for inspection");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn an_empty_or_nul_filled_file_falls_back_to_the_last_good_load() {
    let dir = temp_dir("lastgood");
    let path = dir.join("config.toml");
    std::fs::write(&path, "[recording]\nbuffer_secs = 300\noutput_dir = 'E:\\Clips'\n").unwrap();
    assert_eq!(load_from(&path).recording.buffer_secs, 300);
    // A power cut after a rename can leave the file empty or full of NULs.
    std::fs::write(&path, b"").unwrap();
    let cfg = load_from(&path);
    assert_eq!((cfg.recording.buffer_secs, cfg.recording.output_dir.as_str()), (300, r"E:\Clips"), "empty file");
    std::fs::write(&path, [0u8; 512]).unwrap();
    let cfg = load_from(&path);
    assert_eq!((cfg.recording.buffer_secs, cfg.recording.output_dir.as_str()), (300, r"E:\Clips"), "NUL-filled file");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn keys_from_a_newer_build_survive_a_save() {
    let dir = temp_dir("passthrough");
    let path = dir.join("config.toml");
    let text = "[recording]\nbuffer_secs = 300\nfuture_knob = 1\n[future]\nmode = \"x\"\n[hotkeys]\nold = \"Alt+X\"\n";
    std::fs::write(&path, text).unwrap();
    let mut cfg = load_from(&path);
    assert_eq!(cfg.recording.buffer_secs, 300);
    cfg.recording.buffer_secs = 240;
    cfg.save_to(&path).unwrap();
    let saved: toml::Table = toml::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    assert_eq!(saved["recording"]["future_knob"].as_integer(), Some(1), "unknown key in a known table");
    assert_eq!(saved["future"]["mode"].as_str(), Some("x"), "unknown table");
    assert_eq!(saved["recording"]["buffer_secs"].as_integer(), Some(240), "the change itself landed");
    assert!(!saved.contains_key("hotkeys"), "the retired chord table is not carried");
    assert_eq!(load_from(&path).recording.buffer_secs, 240);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_second_corruption_never_overwrites_the_first_backup() {
    let dir = temp_dir("twobad");
    let path = dir.join("config.toml");
    std::fs::write(&path, "first = = broken").unwrap();
    let _ = load_from(&path);
    std::fs::write(&path, "second = = broken").unwrap();
    let _ = load_from(&path);
    let kept = backups(&path);
    assert!(kept.iter().any(|(_, b)| b == b"first = = broken"), "the first backup survives: {kept:?}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_save_leaves_no_temp_file_and_reloads() {
    let dir = temp_dir("durable");
    let path = dir.join("config.toml");
    let mut cfg = Config::default();
    cfg.recording.buffer_secs = 600;
    cfg.save_to(&path).unwrap();
    cfg.save_to(&path).unwrap();
    assert_eq!(load_from(&path), cfg);
    let names: Vec<String> = std::fs::read_dir(&dir).unwrap().flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect();
    assert!(!names.iter().any(|n| n.ends_with(".tmp")), "{names:?}");
    let _ = std::fs::remove_dir_all(&dir);
}
