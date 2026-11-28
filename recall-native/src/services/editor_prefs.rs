//! Editor defaults remembered across clips (`%APPDATA%\\Recall\\editor.json`,
//! `RECALL_EDITOR_PREFS` overrides the path): Settings > Audio > "Merge audio" (merge on
//! export, mix level offset, prevent clipping), and the library's grouping and columns. Read at launch and when a clip opens.
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::services::library::GroupBy;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EditorPrefs {
    /// Settings > Audio > "Merge audio together on export": every kept track of a clip in one
    /// "Mix" track. On for a new install; the key name is kept so earlier files load.
    #[serde(default = "on", deserialize_with = "on_unless_false")]
    pub mix_down: bool,
    /// Settings > Audio > "Mix level offset": on adds `mix_offset_db` (-12 to +12 dB, 0.5 dB
    /// steps) to the merged Mix track on export, before the limiter. Off for a new install.
    #[serde(default, deserialize_with = "off_unless_true")]
    pub mix_offset_on: bool,
    /// The offset's value, kept while the switch is off. A value out of range or not a number
    /// reads as the nearest step in range (0 for one that is not a number).
    #[serde(default, with = "offset_db")]
    pub mix_offset_db: f32,
    /// Settings > Audio > "Prevent clipping": the -1 dBFS look-ahead limiter on the Mix track.
    /// On for a new install; off, the sum is only clamped at full scale.
    #[serde(default = "on", deserialize_with = "on_unless_false")]
    pub mix_limiter: bool,
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

// A switch saved with a wrong type (another tool's edit) reads as its default; the other
// settings in the file still load.
fn on_unless_false<'de, D: serde::Deserializer<'de>>(d: D) -> Result<bool, D::Error> {
    Ok(serde_json::Value::deserialize(d)?.as_bool().unwrap_or(true))
}

fn off_unless_true<'de, D: serde::Deserializer<'de>>(d: D) -> Result<bool, D::Error> {
    Ok(serde_json::Value::deserialize(d)?.as_bool().unwrap_or(false))
}

/// Settings > Audio > "Merge audio" box: "Mix level offset" (switch + value) and "Prevent
/// clipping", as the editor holds them between reads of `editor.json`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MixBox {
    pub offset_on: bool,
    pub offset_db: f32,
    pub limiter: bool,
}

impl Default for MixBox {
    fn default() -> Self {
        EditorPrefs::default().mix_box()
    }
}

impl MixBox {
    /// The engine's view: the offset only while its switch is on.
    pub fn level(&self) -> crate::edit_model::MixLevel {
        crate::edit_model::MixLevel { offset_db: if self.offset_on { offset_step(self.offset_db) } else { 0.0 }, limiter: self.limiter }
    }
}

/// `value` on the 0.5 dB grid within -12 to +12 dB (0 for one that is not a number).
pub fn offset_step(value: f32) -> f32 {
    use crate::edit_model::{MIX_OFFSET_MAX_DB as MAX, MIX_OFFSET_STEP_DB as STEP};
    if !value.is_finite() {
        return 0.0;
    }
    ((value / STEP).round() * STEP).clamp(-MAX, MAX)
}

mod offset_db {
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(db: &f32, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_f32(super::offset_step(*db))
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<f32, D::Error> {
        let value = serde_json::Value::deserialize(d)?;
        Ok(value.as_f64().map_or(0.0, |v| super::offset_step(v as f32)))
    }
}

impl Default for EditorPrefs {
    fn default() -> Self {
        Self { mix_down: true, mix_offset_on: false, mix_offset_db: 0.0, mix_limiter: true, library_group: GroupBy::default(), library_columns: None }
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
    /// The "Mix level offset" and "Prevent clipping" settings.
    pub fn mix_box(&self) -> MixBox {
        MixBox { offset_on: self.mix_offset_on, offset_db: offset_step(self.mix_offset_db), limiter: self.mix_limiter }
    }

    pub fn load() -> Self {
        path().and_then(|p| std::fs::read(p).ok()).and_then(|bytes| serde_json::from_slice(&bytes).ok()).unwrap_or_default()
    }

    pub fn save(&self) -> Result<(), String> {
        let path = path().ok_or("no settings folder")?;
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| format!("create {}: {e}", dir.display()))?;
        }
        let bytes = crate::services::json_keep::pretty_keeping(&path, self, &["mix_down", "mix_offset_on", "mix_offset_db", "mix_limiter", "library_group", "library_columns"])?;
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
        EditorPrefs { mix_down: false, library_group: GroupBy::Game, ..Default::default() }.save().unwrap();
        assert_eq!(EditorPrefs::load(), EditorPrefs { mix_down: false, library_group: GroupBy::Game, ..Default::default() });
        assert!(std::fs::read_to_string(&file).unwrap().contains("\"library_group\": \"game\""));
        // Back to the default: the key leaves the file, as an earlier release wrote it.
        EditorPrefs { mix_down: false, ..Default::default() }.save().unwrap();
        assert!(!std::fs::read_to_string(&file).unwrap().contains("library_group"));
        // A newer build's grouping: this build shows its default and keeps the merge setting.
        std::fs::write(&file, br#"{"mix_down": false, "library_group": "week"}"#).unwrap();
        assert_eq!(EditorPrefs::load(), EditorPrefs { mix_down: false, ..Default::default() });
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

    #[test]
    fn mix_level_keys_round_trip_and_keep_a_newer_builds_keys() {
        let dir = std::env::temp_dir().join(format!("recall-editor-mix-level-{}", std::process::id()));
        let file = dir.join("editor.json");
        use_test_file(Some(file.clone()));
        // A file from before these settings: defaults (offset off at 0 dB, limiter on).
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(&file, br#"{"mix_down": false, "library_group": "game", "future_key": {"a": 1}}"#).unwrap();
        let old = EditorPrefs::load();
        assert_eq!(old, EditorPrefs { mix_down: false, library_group: GroupBy::Game, ..Default::default() });
        assert_eq!(old.mix_box().level(), crate::edit_model::MixLevel::default());
        let set = EditorPrefs { mix_offset_on: true, mix_offset_db: -4.5, mix_limiter: false, ..old };
        set.save().unwrap();
        assert_eq!(EditorPrefs::load(), set);
        assert_eq!(set.mix_box().level(), crate::edit_model::MixLevel { offset_db: -4.5, limiter: false });
        let text = std::fs::read_to_string(&file).unwrap();
        assert!(text.contains("\"future_key\"") && text.contains("\"mix_offset_db\": -4.5"), "{text}");
        // Off keeps the value but levels at 0 dB.
        assert_eq!(EditorPrefs { mix_offset_on: false, ..set.clone() }.mix_box().level().offset_db, 0.0);
        for (odd, reads) in [("40", 12.0), ("-13", -12.0), ("1.3", 1.5), ("\"x\"", 0.0), ("null", 0.0)] {
            std::fs::write(&file, format!(r#"{{"mix_down": false, "mix_offset_db": {odd}}}"#)).unwrap();
            let p = EditorPrefs::load();
            assert!(!p.mix_down && p.mix_offset_db == reads, "{odd}: {}", p.mix_offset_db);
        }
        use_test_file(None);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_wrong_typed_switch_reads_as_its_default_and_keeps_the_rest() {
        let dir = std::env::temp_dir().join(format!("recall-editor-lenient-{}", std::process::id()));
        let file = dir.join("editor.json");
        use_test_file(Some(file.clone()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(&file, br#"{"mix_down": false, "mix_offset_on": "yes", "mix_offset_db": 2.5, "mix_limiter": 0, "library_group": "game"}"#).unwrap();
        let p = EditorPrefs::load();
        assert!(!p.mix_down && !p.mix_offset_on && p.mix_limiter, "{p:?}");
        assert_eq!((p.mix_offset_db, p.library_group), (2.5, GroupBy::Game));
        std::fs::write(&file, br#"{"mix_down": "no", "mix_offset_on": true, "mix_limiter": null}"#).unwrap();
        let p = EditorPrefs::load();
        assert!(p.mix_down && p.mix_offset_on && p.mix_limiter, "{p:?}");
        use_test_file(None);
        let _ = std::fs::remove_dir_all(dir);
    }
}
