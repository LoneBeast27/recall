//! Editor defaults remembered across clips (`%APPDATA%\\Recall\\editor.json`,
//! `RECALL_EDITOR_PREFS` overrides the path): the last "Mix tracks" choice, used by clips whose
//! edits never set one. Read when a clip opens, written when the choice changes.
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EditorPrefs {
    /// Export mixes tracks into track 1 ("Mix"). On for a new install.
    #[serde(default = "on")]
    pub mix_down: bool,
}

fn on() -> bool {
    true
}

impl Default for EditorPrefs {
    fn default() -> Self {
        Self { mix_down: true }
    }
}

#[cfg(test)]
thread_local! {
    /// Tests run in parallel: each sets its own file, and the rest read and write none.
    static TEST_PATH: std::cell::RefCell<Option<PathBuf>> = const { std::cell::RefCell::new(None) };
}

/// Test only: this thread's prefs file (`None`: no file, the defaults).
#[cfg(test)]
pub fn use_test_file(path: Option<PathBuf>) {
    TEST_PATH.with(|p| *p.borrow_mut() = path);
}

fn path() -> Option<PathBuf> {
    #[cfg(test)]
    return TEST_PATH.with(|p| p.borrow().clone());
    #[allow(unreachable_code)]
    if let Some(path) = std::env::var_os("RECALL_EDITOR_PREFS").filter(|p| !p.is_empty()) {
        return Some(PathBuf::from(path));
    }
    std::env::var_os("APPDATA").filter(|p| !p.is_empty()).map(|root| PathBuf::from(root).join("Recall").join("editor.json"))
}

impl EditorPrefs {
    pub fn load() -> Self {
        path().and_then(|p| std::fs::read(p).ok()).and_then(|bytes| serde_json::from_slice(&bytes).ok()).unwrap_or_default()
    }

    pub fn save(&self) -> Result<(), String> {
        let path = path().ok_or("no settings folder")?;
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| format!("create {}: {e}", dir.display()))?;
        }
        let bytes = serde_json::to_vec_pretty(self).map_err(|e| e.to_string())?;
        crate::services::storage::write_atomic(&path, &bytes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_last_choice_is_kept_and_a_new_install_mixes() {
        let dir = std::env::temp_dir().join(format!("recall-editor-prefs-{}", std::process::id()));
        let file = dir.join("editor.json");
        use_test_file(Some(file));
        assert!(EditorPrefs::load().mix_down, "nothing saved: on");
        EditorPrefs { mix_down: false }.save().unwrap();
        assert!(!EditorPrefs::load().mix_down);
        use_test_file(None);
        let _ = std::fs::remove_dir_all(dir);
    }
}
