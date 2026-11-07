//! Hand edits of config.toml while Recall runs. The controller loads the file once and
//! patches its in-memory copy, so the next Settings click (or Alt+P / Alt+O) used to write
//! that copy over an edit made in the meantime. Before a patch, a file whose modified time
//! or size moved since the controller last read or wrote it is read again and becomes the
//! base the patch merges into (with the load-time repair of `config_validate`).

use std::path::Path;
use std::time::SystemTime;

use crate::config::Config;
use crate::log;
use crate::record_config::ConfigStore;

/// What the file looked like when the controller last read or wrote it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FileStamp {
    modified: Option<SystemTime>,
    len: u64,
}

impl FileStamp {
    /// `None` when the file is missing or unreadable.
    pub fn of(path: &Path) -> Option<FileStamp> {
        let meta = std::fs::metadata(path).ok()?;
        Some(FileStamp { modified: meta.modified().ok(), len: meta.len() })
    }
}

/// The config a patch should merge into when config.toml changed on disk since `seen`
/// (`None`: unchanged, use the one in memory). `seen` moves to the file's new stamp once it is
/// read. A file that is gone, or does not parse (an edit still being typed), is not loaded:
/// the settings in memory stay the base, and the file is not moved aside.
pub fn base_for_patch(store: &ConfigStore, seen: &mut Option<FileStamp>) -> Option<Config> {
    let now = FileStamp::of(store.path())?;
    if Some(now) == *seen {
        return None;
    }
    let text = std::fs::read_to_string(store.path()).ok()?;
    if let Err(e) = toml::from_str::<Config>(&text) {
        log!("config: config.toml changed on disk but does not load ({e}); keeping the settings in use");
        return None;
    }
    *seen = Some(now);
    log!("config: config.toml changed on disk; applying the edit before this change");
    Some(store.load())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::record_config::apply_patch;
    use serde_json::json;

    fn store(tag: &str) -> ConfigStore {
        let dir = std::env::temp_dir().join(format!(
            "recall-hdr-watch-{tag}-{}-{}",
            std::process::id(),
            SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        ConfigStore::at(dir.join("config.toml"))
    }

    #[test]
    fn a_hand_edit_made_while_running_survives_the_next_patch() {
        let store = store("edit");
        let running = Config::default();
        store.save(&running).unwrap();
        let mut seen = FileStamp::of(store.path());
        let first = seen;
        let text = std::fs::read_to_string(store.path()).unwrap();
        assert!(text.contains("cpu_mask = \"auto\""));
        std::fs::write(store.path(), text.replace("cpu_mask = \"auto\"", "cpu_mask = \"ffff0000\"")).unwrap();
        let base = base_for_patch(&store, &mut seen).unwrap_or_else(|| running.clone());
        assert_ne!(seen, first, "the edit's stamp is remembered");
        let out = apply_patch(&base, &json!({"overlay": {"enabled": true}})).unwrap();
        store.save(&out.config).unwrap();
        let back = store.load();
        assert_eq!(back.recording.cpu_mask, "ffff0000", "the hand edit is kept");
        assert!(back.overlay.enabled, "the patch applied on top");
    }

    #[test]
    fn an_unchanged_file_is_not_read_again() {
        let store = store("same");
        let running = Config::default();
        store.save(&running).unwrap();
        let mut seen = FileStamp::of(store.path());
        assert!(base_for_patch(&store, &mut seen).is_none());
    }

    #[test]
    fn an_unreadable_edit_keeps_the_settings_in_memory() {
        let store = store("broken");
        let mut running = Config::default();
        running.recording.fps = 120;
        store.save(&running).unwrap();
        let mut seen = FileStamp::of(store.path());
        std::fs::write(store.path(), "[recording]\nfps = = 1\n").unwrap();
        assert!(base_for_patch(&store, &mut seen).is_none(), "half-typed edits are not loaded");
        assert!(store.path().exists(), "and the file is not moved to .bad behind the user's back");
    }
}
