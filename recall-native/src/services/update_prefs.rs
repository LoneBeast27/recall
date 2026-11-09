//! "Check for updates automatically": the app's own setting, default on, kept beside the
//! skipped version in `%LOCALAPPDATA%\Recall\updates\settings.json`.
use std::path::{Path, PathBuf};

pub(super) fn settings_file() -> Option<PathBuf> {
    std::env::var_os("LOCALAPPDATA").map(|p| PathBuf::from(p).join("Recall").join("updates").join("settings.json"))
}

/// On unless the file says `"check_automatically": false`.
pub(super) fn load_auto(file: Option<&Path>) -> bool {
    file.and_then(|f| std::fs::read(f).ok())
        .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok())
        .and_then(|v| v.get("check_automatically").and_then(serde_json::Value::as_bool))
        .unwrap_or(true)
}

pub(super) fn save_auto(file: &Path, on: bool) -> Result<(), String> {
    if let Some(dir) = file.parent() {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    std::fs::write(file, serde_json::json!({ "check_automatically": on }).to_string()).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn automatic_checks_default_on_and_persist() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("updates").join("settings.json");
        assert!(load_auto(None), "no LOCALAPPDATA: default on");
        assert!(load_auto(Some(&file)), "no file yet: default on");
        save_auto(&file, false).unwrap();
        assert!(!load_auto(Some(&file)));
        save_auto(&file, true).unwrap();
        assert!(load_auto(Some(&file)));
        std::fs::write(&file, b"not json").unwrap();
        assert!(load_auto(Some(&file)), "an unreadable file falls back to on");
    }
}
