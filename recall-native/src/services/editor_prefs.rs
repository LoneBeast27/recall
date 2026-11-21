//! Editor defaults remembered across clips (`%APPDATA%\\Recall\\editor.json`,
//! `RECALL_EDITOR_PREFS` overrides the path): the last "Mix tracks" choice, used by clips whose
//! edits never set one. Read when a clip opens, written when the choice changes.
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::services::library::GroupBy;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EditorPrefs {
    /// Export mixes tracks into track 1 ("Mix"). On for a new install.
    #[serde(default = "on")]
    pub mix_down: bool,
    /// Library "Group by". Left out of the file while it is the default, so files stay as
    /// earlier releases wrote them; a word this build does not know reads as the default.
    #[serde(default, with = "group_word", skip_serializing_if = "is_default_group")]
    pub library_group: GroupBy,
    /// Library tiles across: absent = automatic; 1 to 8 = that many. Any other value reads as
    /// automatic.
    #[serde(default, with = "columns_number", skip_serializing_if = "Option::is_none")]
    pub library_columns: Option<usize>,
}

mod columns_number {
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(columns: &Option<usize>, s: S) -> Result<S::Ok, S::Error> {
        match columns {
            Some(n) => s.serialize_u64(*n as u64),
            None => s.serialize_none(),
        }
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Option<usize>, D::Error> {
        let value = serde_json::Value::deserialize(d)?;
        Ok(value.as_u64().filter(|n| (1..=8).contains(n)).map(|n| n as usize))
    }
}

fn is_default_group(group: &GroupBy) -> bool {
    *group == GroupBy::default()
}

mod group_word {
    use super::GroupBy;
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(group: &GroupBy, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(group.word())
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<GroupBy, D::Error> {
        let value = serde_json::Value::deserialize(d)?;
        Ok(value.as_str().and_then(GroupBy::from_word).unwrap_or_default())
    }
}

fn on() -> bool {
    true
}

impl Default for EditorPrefs {
    fn default() -> Self {
        Self { mix_down: true, library_group: GroupBy::default(), library_columns: None }
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
    crate::user_dirs::appdata().map(|root| crate::user_dirs::checked(root.join("Recall").join("editor.json")))
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
        let bytes = crate::services::json_keep::pretty_keeping(&path, self, &["mix_down", "library_group", "library_columns"])?;
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
        EditorPrefs { mix_down: false, ..Default::default() }.save().unwrap();
        assert!(!EditorPrefs::load().mix_down);
        use_test_file(None);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn library_group_is_kept_and_an_unknown_word_reads_as_the_default() {
        let dir = std::env::temp_dir().join(format!("recall-editor-group-{}", std::process::id()));
        let file = dir.join("editor.json");
        use_test_file(Some(file.clone()));
        assert_eq!(EditorPrefs::load().library_group, GroupBy::None, "nothing saved: no groups");
        EditorPrefs { mix_down: false, library_group: GroupBy::Game, library_columns: None }.save().unwrap();
        assert_eq!(EditorPrefs::load(), EditorPrefs { mix_down: false, library_group: GroupBy::Game, library_columns: None });
        assert!(std::fs::read_to_string(&file).unwrap().contains("\"library_group\": \"game\""));
        // Back to the default: the key leaves the file, as an earlier release wrote it.
        EditorPrefs { mix_down: false, library_group: GroupBy::None, library_columns: None }.save().unwrap();
        assert!(!std::fs::read_to_string(&file).unwrap().contains("library_group"));
        // A newer build's grouping: this build shows its default and keeps "Mix tracks".
        std::fs::write(&file, br#"{"mix_down": false, "library_group": "week"}"#).unwrap();
        assert_eq!(EditorPrefs::load(), EditorPrefs { mix_down: false, library_group: GroupBy::None, library_columns: None });
        std::fs::write(&file, br#"{"mix_down": false, "library_group": 7}"#).unwrap();
        assert!(!EditorPrefs::load().mix_down, "a wrong type loses only the grouping");
        use_test_file(None);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn library_columns_are_kept_and_odd_values_read_as_automatic() {
        let dir = std::env::temp_dir().join(format!("recall-editor-columns-{}", std::process::id()));
        let file = dir.join("editor.json");
        use_test_file(Some(file.clone()));
        assert_eq!(EditorPrefs::load().library_columns, None, "nothing saved: automatic");
        let five = EditorPrefs { library_columns: Some(5), ..Default::default() };
        five.save().unwrap();
        assert_eq!(EditorPrefs::load(), five);
        assert!(std::fs::read_to_string(&file).unwrap().contains("\"library_columns\": 5"));
        EditorPrefs::default().save().unwrap();
        assert!(!std::fs::read_to_string(&file).unwrap().contains("library_columns"), "automatic writes no key");
        for odd in ["0", "9", "-1", "\"4\"", "null"] {
            std::fs::write(&file, format!(r#"{{"mix_down": false, "library_columns": {odd}}}"#)).unwrap();
            assert_eq!(EditorPrefs::load(), EditorPrefs { mix_down: false, ..Default::default() }, "{odd}");
        }
        use_test_file(None);
        let _ = std::fs::remove_dir_all(dir);
    }
}
