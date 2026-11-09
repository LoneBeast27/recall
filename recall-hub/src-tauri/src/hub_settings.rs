//! The hub's own saved settings: `<app local data>\hub-settings.json`
//! (`%LOCALAPPDATA%\<identifier>`). Today it holds one key, `clipFolder`, the folder
//! the clips page lists when `RECALL_OUT_DIR` is not set. The hub only reads it;
//! nothing in the hub writes it.

use std::path::{Path, PathBuf};

pub const SETTINGS_FILE: &str = "hub-settings.json";
const CLIP_FOLDER_KEY: &str = "clipFolder";

/// `<local app data dir>\hub-settings.json`.
pub fn settings_path(local_data_dir: &Path) -> PathBuf {
    local_data_dir.join(SETTINGS_FILE)
}

/// The saved clip folder, if one is set (a missing file, bad JSON or a blank value count as unset).
pub fn saved_clip_folder(path: &Path) -> Option<String> {
    let bytes = std::fs::read(path).ok()?;
    let value: serde_json::Value = serde_json::from_slice(&bytes).ok()?;
    value
        .get(CLIP_FOLDER_KEY)
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
}

/// The folder the clips page lists: `RECALL_OUT_DIR`, else the saved `clipFolder`,
/// else `<Videos>\Recall`; `None` when none of them is known.
pub fn clip_dir(env: Option<PathBuf>, saved: Option<String>, videos: Option<PathBuf>) -> Option<PathBuf> {
    env.or_else(|| saved.map(PathBuf::from)).or_else(|| videos.map(|v| v.join("Recall")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lookup_order() {
        let (env, saved, videos) = (PathBuf::from(r"E:\Env"), r"D:\Saved".to_string(), PathBuf::from(r"C:\Videos"));
        assert_eq!(clip_dir(Some(env.clone()), Some(saved.clone()), Some(videos.clone())), Some(env));
        assert_eq!(clip_dir(None, Some(saved.clone()), Some(videos.clone())), Some(PathBuf::from(saved)));
        assert_eq!(clip_dir(None, None, Some(videos.clone())), Some(videos.join("Recall")));
        assert_eq!(clip_dir(None, None, None), None);
    }

    #[test]
    fn reading_never_writes_and_blank_is_unset() {
        let dir = std::env::temp_dir().join(format!("recall-hub-settings-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let path = settings_path(&dir);
        assert_eq!(saved_clip_folder(&path), None, "fresh install: no file");
        assert!(!dir.exists(), "fresh install: reading creates nothing");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(&path, r#"{"clipFolder": "  "}"#).unwrap();
        assert_eq!(saved_clip_folder(&path), None, "blank counts as unset");
        std::fs::write(&path, r#"{"clipFolder": "D:\\Clips", "other": 1}"#).unwrap();
        assert_eq!(saved_clip_folder(&path).as_deref(), Some(r"D:\Clips"));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
