//! The hub's own saved settings: `<app local data>\hub-settings.json`
//! (`%LOCALAPPDATA%\<identifier>`). Today it holds one key, `clipFolder`, the folder
//! the clips page lists when `RECALL_OUT_DIR` is not set.

use std::path::{Path, PathBuf};

pub const SETTINGS_FILE: &str = "hub-settings.json";
const CLIP_FOLDER_KEY: &str = "clipFolder";

/// `<local app data base>\<identifier>\hub-settings.json`.
pub fn settings_path(local_data_dir: &Path) -> PathBuf {
    local_data_dir.join(SETTINGS_FILE)
}

fn read(path: &Path) -> serde_json::Map<String, serde_json::Value> {
    std::fs::read(path)
        .ok()
        .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok())
        .and_then(|v| v.as_object().cloned())
        .unwrap_or_default()
}

/// The saved clip folder, if one is set (a blank value counts as unset).
pub fn saved_clip_folder(path: &Path) -> Option<String> {
    read(path)
        .get(CLIP_FOLDER_KEY)
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
}

/// Saves `folder` as the clip folder, keeping every other key in the file.
pub fn save_clip_folder(path: &Path, folder: &str) -> std::io::Result<()> {
    let mut map = read(path);
    map.insert(CLIP_FOLDER_KEY.into(), serde_json::Value::String(folder.into()));
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let text = serde_json::to_string_pretty(&serde_json::Value::Object(map)).map_err(std::io::Error::other)?;
    std::fs::write(path, text)
}

/// Which folder (if any) the legacy carry-over should save: only when nothing is
/// saved yet, the hub ran under its old identifier, and the legacy folder exists.
pub fn legacy_folder_to_save<'a>(saved: Option<&str>, ran_before: bool, legacy: &'a str, legacy_exists: bool) -> Option<&'a str> {
    (saved.is_none() && ran_before && legacy_exists).then_some(legacy)
}

#[cfg(test)]
mod tests {
    use super::*;

    const LEGACY: &str = r"Z:\Old Clips";

    #[test]
    fn decision() {
        assert_eq!(legacy_folder_to_save(None, true, LEGACY, true), Some(LEGACY), "no setting + legacy exists -> saved");
        assert_eq!(legacy_folder_to_save(Some(r"D:\Mine"), true, LEGACY, true), None, "saved setting stays");
        assert_eq!(legacy_folder_to_save(None, true, LEGACY, false), None, "legacy missing -> no write");
        assert_eq!(legacy_folder_to_save(None, false, LEGACY, true), None, "new install -> no write");
    }

    #[test]
    fn save_and_read_keep_other_keys() {
        let dir = std::env::temp_dir().join(format!("recall-hub-settings-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let path = settings_path(&dir);
        assert_eq!(saved_clip_folder(&path), None);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(&path, r#"{"other": 1, "clipFolder": "  "}"#).unwrap();
        assert_eq!(saved_clip_folder(&path), None, "blank counts as unset");
        save_clip_folder(&path, LEGACY).unwrap();
        assert_eq!(saved_clip_folder(&path).as_deref(), Some(LEGACY));
        let v: serde_json::Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(v["other"], 1);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
