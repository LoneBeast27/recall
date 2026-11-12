//! A bad or locked island-hud.json never wipes the placed spots (deep-audit O3).
use super::*;
use crate::spot_edit::{Edge, SavedSpot};

fn temp_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("recall-island-persist-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

#[test]
fn a_valid_spot_survives_an_entry_from_a_newer_build() {
    let dir = temp_dir("entry");
    let path = dir.join("island-hud.json");
    let text = r#"{"games":{},"seq":3,"spots":{"valorant":{"x":0.5,"y":0.0,"edge":"top"},"apex":{"x":0.5,"y":0.5,"edge":"corner"}}}"#;
    std::fs::write(&path, text).unwrap();
    let mut cache = HudCache::load(Some(path.clone()));
    cache.add("valorant@9", HudGrid { cols: 8, rows: 1, grid: vec![2] });
    let back = HudCache::load(Some(path.clone()));
    assert_eq!(back.spot("valorant"), Some(SavedSpot { x: 0.5, y: 0.0, edge: Edge::Top }), "one unknown entry drops only itself");
    let kept: Vec<String> = std::fs::read_dir(&dir).unwrap().flatten().map(|e| e.file_name().to_string_lossy().into_owned()).filter(|n| n.starts_with("island-hud.json.bad")).collect();
    assert_eq!(kept.len(), 1, "the original is kept before the lossy rewrite: {kept:?}");
    assert_eq!(std::fs::read_to_string(dir.join(&kept[0])).unwrap(), text);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn an_unreadable_file_is_left_untouched() {
    use std::os::windows::fs::OpenOptionsExt;
    let dir = temp_dir("locked");
    let path = dir.join("island-hud.json");
    let text = r#"{"games":{},"seq":1,"spots":{"valorant":{"x":0.5,"y":0.0,"edge":"top"}}}"#;
    std::fs::write(&path, text).unwrap();
    let held = std::fs::OpenOptions::new().read(true).share_mode(0).open(&path).unwrap();
    let mut cache = HudCache::load(Some(path.clone()));
    drop(held);
    cache.add("valorant@9", HudGrid { cols: 8, rows: 1, grid: vec![2] });
    cache.set_spot("apex", Some(SavedSpot { x: 0.5, y: 1.0, edge: Edge::Bottom }));
    assert_eq!(std::fs::read_to_string(&path).unwrap(), text, "no save after a failed read");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_truncated_file_is_kept_as_bad() {
    let dir = temp_dir("truncated");
    let path = dir.join("island-hud.json");
    let text = r#"{"games":{},"spots":{"valorant":{"x":0.5,"y":0.0,"edge":"top"}"#;
    std::fs::write(&path, text).unwrap();
    let mut cache = HudCache::load(Some(path.clone()));
    cache.add("valorant@9", HudGrid { cols: 8, rows: 1, grid: vec![2] });
    let kept: Vec<String> = std::fs::read_dir(&dir).unwrap().flatten().map(|e| e.file_name().to_string_lossy().into_owned()).filter(|n| n.starts_with("island-hud.json.bad")).collect();
    assert_eq!(kept.len(), 1, "{kept:?}");
    assert_eq!(std::fs::read_to_string(dir.join(&kept[0])).unwrap(), text);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn island_sizes_persist_per_game_and_old_files_load_at_100_percent() {
    let dir = temp_dir("sizes");
    let path = dir.join("island-hud.json");
    // A file from a build before sizes: everything loads, every game at 100%.
    std::fs::write(&path, r#"{"games":{},"seq":3,"spots":{"valorant":{"x":0.5,"y":0.0,"edge":"top"}}}"#).unwrap();
    let mut cache = HudCache::load(Some(path.clone()));
    assert_eq!(cache.size("valorant"), 1.0);
    cache.set_size("valorant", 1.25);
    cache.set_size("apex", 0.8);
    let mut back = HudCache::load(Some(path.clone()));
    assert_eq!((back.size("valorant"), back.size("apex")), (1.25, 0.8));
    assert_eq!(back.spot("valorant"), Some(SavedSpot { x: 0.5, y: 0.0, edge: Edge::Top }), "the spot stays beside the size");
    back.set_size("apex", 1.0);
    let text = std::fs::read_to_string(&path).unwrap();
    assert!(!text.contains("apex"), "100% is not kept: {text}");
    back.set_size("My Game - Window Title", 1.2);
    assert_eq!(HudCache::load(Some(path.clone())).size("My Game - Window Title"), 1.0, "a window title is never kept");
    // A size out of range (a hand edit) drops only itself.
    std::fs::write(&path, r#"{"games":{},"seq":3,"sizes":{"valorant":9.0,"apex":1.1}}"#).unwrap();
    let odd = HudCache::load(Some(path.clone()));
    assert_eq!((odd.size("valorant"), odd.size("apex")), (1.0, 1.1));
    let _ = std::fs::remove_dir_all(&dir);
}
