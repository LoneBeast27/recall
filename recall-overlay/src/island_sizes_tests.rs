use super::*;
use crate::dynamic::HudCache;
use crate::spot_edit::{Edge, SavedSpot};

fn temp_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("recall-island-sizes-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

#[test]
fn sizes_persist_per_game_and_100_percent_or_a_title_is_never_kept() {
    let dir = temp_dir("persist");
    let path = dir.join("island-sizes.json");
    let mut store = SizeStore::load(Some(path.clone()));
    assert_eq!(store.size("valorant"), 1.0, "no file: every game at 100%");
    store.set_size("valorant", 1.25);
    store.set_size("apex", 0.8);
    let mut back = SizeStore::load(Some(path.clone()));
    assert_eq!((back.size("valorant"), back.size("apex")), (1.25, 0.8));
    back.set_size("apex", 1.0);
    back.set_size("My Game - Window Title", 1.2);
    let text = std::fs::read_to_string(&path).unwrap();
    assert!(!text.contains("apex") && !text.contains("Title"), "100% and titles are not kept: {text}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_bad_size_drops_only_itself_and_the_original_is_kept() {
    let dir = temp_dir("bad");
    let path = dir.join("island-sizes.json");
    let text = r#"{"seq":3,"sizes":{"valorant":9.0,"apex":1.1,"cs2":"big"}}"#;
    std::fs::write(&path, text).unwrap();
    let store = SizeStore::load(Some(path.clone()));
    assert_eq!((store.size("valorant"), store.size("apex"), store.size("cs2")), (1.0, 1.1, 1.0));
    let kept = std::fs::read_dir(&dir).unwrap().flatten().filter(|e| e.file_name().to_string_lossy().starts_with("island-sizes.json.bad")).count();
    assert_eq!(kept, 1, "the original is kept before a lossy rewrite");
    std::fs::write(&path, "not json").unwrap();
    assert_eq!(SizeStore::load(Some(path.clone())).size("apex"), 1.0, "a broken file loads empty");
    let _ = std::fs::remove_dir_all(&dir);
}

/// The island-hud.json shape of v0.1.5 (`CacheFile`): it reads its own
/// fields and writes them back whole, dropping any key it does not know.
#[derive(Serialize, Deserialize)]
struct V015CacheFile {
    games: BTreeMap<String, Value>,
    #[serde(default)]
    seq: u64,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    spots: BTreeMap<String, SavedSpot>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    spots_seq: BTreeMap<String, u64>,
}

#[test]
fn a_rollback_to_v015_saving_the_hud_cache_keeps_every_size() {
    let dir = temp_dir("rollback");
    let (hud, sizes) = (dir.join("island-hud.json"), dir.join("island-sizes.json"));
    // This build: a placed spot and a size.
    let mut cache = HudCache::load(Some(hud.clone()));
    cache.set_spot("valorant", Some(SavedSpot { x: 0.5, y: 0.0, edge: Edge::Top }));
    SizeStore::load(Some(sizes.clone())).set_size("valorant", 1.3);
    let before = std::fs::read(&sizes).unwrap();
    assert!(!std::fs::read_to_string(&hud).unwrap().contains("size"), "no size goes into island-hud.json");
    // v0.1.5 loads island-hud.json and saves it back over the file.
    let old: V015CacheFile = serde_json::from_str(&std::fs::read_to_string(&hud).unwrap()).expect("v0.1.5 reads the file");
    std::fs::write(&hud, serde_json::to_string(&old).unwrap()).unwrap();
    // Back on this build: the size file is untouched and the size is there.
    assert_eq!(std::fs::read(&sizes).unwrap(), before, "the sizes file is intact");
    assert_eq!(SizeStore::load(Some(sizes.clone())).size("valorant"), 1.3);
    assert_eq!(HudCache::load(Some(hud.clone())).spot("valorant").map(|s| s.edge), Some(Edge::Top));
    let _ = std::fs::remove_dir_all(&dir);
}
