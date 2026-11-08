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
