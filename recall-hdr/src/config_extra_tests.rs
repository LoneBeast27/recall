//! A config written by Recall 0.1.0 survives this build's load and save; keys this build does
//! not know survive at any depth; a value that does not read comes back from the update's
//! settings backup with a notice (the maintainer's rule: "ensure updates always save a user
//! configuration so there is no breaking setup per se").

use std::path::{Path, PathBuf};

use super::super::{load_from, Config, Override};
use crate::types::RuleKind;

/// A config shaped as Recall 0.1.0 (cfbc887) wrote it: its defaults (the hdr-testcard override,
/// the game + microphone sources), neutral device names and paths, serialized by the same
/// `toml::to_string_pretty` over the same struct order.
fn v010_text() -> String {
    let mut cfg = Config { overrides: vec![Override { exe: "hdr-testcard".into(), kind: RuleKind::HdrGame, clamp: None }], ..Config::default() };
    cfg.recording.output_dir = r"D:\Clips\Recall".into();
    cfg.recording.audio_sources = crate::record_audio_sources::default_sources();
    cfg.recording.audio_sources[1].device = "USB Microphone".into();
    cfg.recording.audio_sources[1].device_id = "{0.0.1.00000000}.{11111111-2222-3333-4444-555555555555}".into();
    cfg.storage.cap_gb = 200;
    toml::to_string_pretty(&cfg).unwrap()
}

fn dir(tag: &str) -> PathBuf {
    crate::config_extra_tests_dir(tag)
}

fn save_load(path: &Path) -> String {
    let cfg = load_from(path);
    cfg.save_to(path).unwrap();
    std::fs::read_to_string(path).unwrap()
}

#[test]
fn a_v010_config_round_trips_byte_for_byte() {
    let root = dir("v010");
    let _home = crate::user_dirs::inject(&root);
    let path = root.join("config.toml");
    let text = v010_text();
    std::fs::write(&path, &text).unwrap();
    let saved = save_load(&path);
    // Debug builds keep the testcard (its removal is the one intended migration, release only).
    assert_eq!(saved, text, "nothing of the user's file changed");
    assert_eq!(save_load(&path), text, "and again");
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn unknown_keys_survive_at_every_depth() {
    let root = dir("future");
    let _home = crate::user_dirs::inject(&root);
    let path = root.join("config.toml");
    let mut text = v010_text();
    text = text.replacen("[recording]\n", "[recording]\nfuture_codec_tuning = \"x\"\n", 1);
    text = text.replacen("exe = \"hdr-testcard\"\n", "exe = \"hdr-testcard\"\nfuture_priority = 3\n", 1);
    text.push_str("\n[future_section]\nflag = true\n\n[future_section.deep]\nlevel = 2\n");
    std::fs::write(&path, &text).unwrap();
    let mut cfg = load_from(&path);
    cfg.storage.cap_gb = 300; // a real change, saved by this build
    cfg.save_to(&path).unwrap();
    let saved: toml::Table = std::fs::read_to_string(&path).unwrap().parse().unwrap();
    assert_eq!(saved["storage"]["cap_gb"].as_integer(), Some(300));
    assert_eq!(saved["recording"]["future_codec_tuning"].as_str(), Some("x"), "one level in");
    assert_eq!(saved["overrides"][0]["future_priority"].as_integer(), Some(3), "inside an [[overrides]] entry");
    assert_eq!(saved["future_section"]["deep"]["level"].as_integer(), Some(2), "two levels in a new section");
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn an_unknown_key_of_a_removed_entry_goes_with_it() {
    let root = dir("removed");
    let _home = crate::user_dirs::inject(&root);
    let path = root.join("config.toml");
    let text = v010_text().replacen("exe = \"hdr-testcard\"\n", "exe = \"hdr-testcard\"\nfuture_priority = 3\n", 1);
    std::fs::write(&path, &text).unwrap();
    let mut cfg = load_from(&path);
    cfg.overrides.clear();
    cfg.save_to(&path).unwrap();
    let saved = std::fs::read_to_string(&path).unwrap();
    assert!(!saved.contains("future_priority") && !saved.contains("hdr-testcard"), "{saved}");
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn a_value_that_does_not_read_comes_back_from_the_update_backup_with_a_notice() {
    let root = dir("restore");
    let _home = crate::user_dirs::inject(&root);
    let backup = root.join(r"AppData\Local\Recall\updates\config-backup-0.1.0-20261110-120000\roaming\recall-hdr\config.toml");
    std::fs::create_dir_all(backup.parent().unwrap()).unwrap();
    std::fs::write(&backup, v010_text()).unwrap();
    let path = root.join("config.toml");
    // The update's new build finds a value it cannot read (here: a cap that is not a number).
    std::fs::write(&path, v010_text().replace("cap_gb = 200", "cap_gb = \"lots\"")).unwrap();
    let cfg = load_from(&path);
    assert_eq!(cfg.storage.cap_gb, 200, "restored from the backup, not the default");
    assert_eq!(cfg.recording.output_dir, r"D:\Clips\Recall", "the rest of the file as it was");
    let notes = crate::config_repairs::take(&path);
    assert!(notes.iter().any(|n| n.contains("storage.cap_gb") && n.contains("restored from the settings backup")), "{notes:?}");
    // A file that is not TOML at all and has no last good copy: the backup, whole.
    let other = root.join("other.toml");
    std::fs::write(&other, "\u{0}\u{0}garbage [[[").unwrap();
    assert_eq!(load_from(&other).storage.cap_gb, 200);
    assert!(crate::config_repairs::take(&other).iter().any(|n| n.contains("restored from the settings backup")));
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn without_a_backup_a_bad_value_is_reset_and_said() {
    let root = dir("nobackup");
    let _home = crate::user_dirs::inject(&root);
    let path = root.join("config.toml");
    std::fs::write(&path, v010_text().replace("cap_gb = 200", "cap_gb = \"lots\"")).unwrap();
    let cfg = load_from(&path);
    assert_eq!(cfg.storage.cap_gb, Config::default().storage.cap_gb);
    assert!(crate::config_repairs::take(&path).iter().any(|n| n.contains("reset to the default")));
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn an_unknown_key_survives_a_save_after_a_bad_value_was_salvaged() {
    let root = dir("salvaged");
    let _home = crate::user_dirs::inject(&root);
    let path = root.join("config.toml");
    let text = v010_text().replace("cap_gb = 200", "cap_gb = \"lots\"").replacen("[recording]\n", "[recording]\nfuture_codec_tuning = \"x\"\n", 1)
        + "\n[future_section]\nflag = true\n";
    std::fs::write(&path, &text).unwrap();
    let cfg = load_from(&path); // salvaged: the file on disk still holds the bad value
    cfg.save_to(&path).unwrap();
    let saved: toml::Table = std::fs::read_to_string(&path).unwrap().parse().unwrap();
    assert_eq!(saved["recording"]["future_codec_tuning"].as_str(), Some("x"), "one level in");
    assert_eq!(saved["future_section"]["flag"].as_bool(), Some(true), "a new section");
    assert!(saved["storage"]["cap_gb"].as_integer().is_some(), "the bad value is not carried");
    let _ = std::fs::remove_dir_all(&root);
}
