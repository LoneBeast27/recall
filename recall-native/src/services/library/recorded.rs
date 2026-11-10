//! Clips Recall recorded outside its recordings folders: a startup `-OutDir` override
//! (`RECALL_OUT_DIR` for the recorder only) saves into a folder that is neither the recordings
//! folder nor a former one, so `imports::is_owned` could not tell those clips from another
//! recorder's, and Delete only hid them. Each such save is listed here by its exact path (never
//! its folder: other files there stay foreign) at `<imports base>\recorded.json`; a clip that
//! went to the Recycle Bin leaves the list.
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use super::folders::folder_key;

/// Folder keys of the listed clips; loaded on first use.
static SET: Mutex<Option<BTreeSet<String>>> = Mutex::new(None);

fn path() -> Option<PathBuf> {
    super::imports::base_dir().map(|dir| dir.join("recorded.json"))
}

fn load() -> BTreeSet<String> {
    let Some(path) = path() else { return BTreeSet::new() };
    let Ok(bytes) = std::fs::read(&path) else { return BTreeSet::new() };
    let value: serde_json::Value = serde_json::from_slice(&bytes).unwrap_or_else(|_| {
        // Kept for inspection (and for an update's restore) before the next note rewrites it.
        let _ = crate::services::durable::quarantine(&path, std::time::SystemTime::now(), crate::services::durable::Keep::Copy);
        serde_json::Value::default()
    });
    value.get("clips").and_then(|c| c.as_array()).map(|a| a.iter().filter_map(|s| s.as_str()).map(folder_key).collect()).unwrap_or_default()
}

fn with_set<T>(work: impl FnOnce(&mut BTreeSet<String>) -> T) -> T {
    let mut guard = SET.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    work(guard.get_or_insert_with(load))
}

fn save(set: &BTreeSet<String>) {
    let Some(path) = path() else { return };
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let value = crate::services::json_keep::with_unknown_keys(&path, serde_json::json!({ "version": 1, "clips": set }), &["version", "clips"]);
    let bytes = serde_json::to_vec_pretty(&value).unwrap_or_default();
    if let Err(error) = crate::services::durable::write_durable(&path, &bytes) {
        eprintln!("recorded clips: save {}: {error}", path.display());
    }
}

/// The recorder saved `clip` (a fresh save outside the recordings folders): Recall's own.
pub fn note(clip: &str) {
    let key = folder_key(clip);
    if key.is_empty() {
        return;
    }
    with_set(|set| {
        if set.insert(key) {
            save(set);
        }
    });
}

/// Whether Recall recorded exactly this file.
pub fn contains(clip: &Path) -> bool {
    let key = folder_key(&clip.to_string_lossy());
    with_set(|set| set.contains(&key))
}

/// These clips went to the Recycle Bin: a later file at the same path is not Recall's.
pub fn forget(clips: &[PathBuf]) {
    with_set(|set| {
        let before = set.len();
        for clip in clips {
            set.remove(&folder_key(&clip.to_string_lossy()));
        }
        if set.len() != before {
            save(set);
        }
    });
}
