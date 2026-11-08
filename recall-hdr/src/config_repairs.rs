//! Load-time repairs of config.toml (`config_validate`) reaching the app. A repair used to be a
//! log line only, so a storage cap or a second mic could quietly change. `config::load_from`
//! leaves the notes here per file; the controller takes them after its own load and sends them
//! with the `config` event (`config_repaired`), and the app shows one toast per load.

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde_json::{json, Value};

/// The notes of the last load of each file, until the controller takes them.
static LOAD_REPAIRS: Mutex<Vec<(PathBuf, Vec<String>)>> = Mutex::new(Vec::new());

/// Remember `notes` as the repairs of the load of `path` that just ran (replacing older ones).
pub fn remember(path: &Path, notes: Vec<String>) {
    let Ok(mut all) = LOAD_REPAIRS.lock() else { return };
    all.retain(|(p, _)| p != path);
    if !notes.is_empty() {
        all.push((path.to_path_buf(), notes));
    }
}

/// The repairs the last load of `path` made (empty after the first take).
pub fn take(path: &Path) -> Vec<String> {
    let Ok(mut all) = LOAD_REPAIRS.lock() else { return Vec::new() };
    let at = all.iter().position(|(p, _)| p == path);
    at.map(|i| all.remove(i).1).unwrap_or_default()
}

/// What the `config` event says about the last load. `id` changes with every load that repaired
/// something (the app toasts once per id); 0 = nothing to say.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ConfigRepairs {
    pub id: u64,
    pub notes: Vec<String>,
}

impl ConfigRepairs {
    /// After a load: its notes replace the old ones; a non-empty set gets a new id (`now_ms`,
    /// unique across controller restarts, so a running app does not mistake it for one it showed).
    pub fn loaded(&mut self, notes: Vec<String>, now_ms: u64) {
        if notes.is_empty() {
            *self = ConfigRepairs::default();
            return;
        }
        self.id = now_ms.max(self.id + 1);
        self.notes = notes;
    }

    /// The repairs of the load of `path` that just ran (`config::load_from` left them).
    pub fn after_load(path: &Path) -> ConfigRepairs {
        let mut repairs = ConfigRepairs::default();
        repairs.loaded(take(path), now_ms());
        repairs
    }

    pub fn json(&self) -> Value {
        json!({ "id": self.id, "count": self.notes.len(), "notes": self.notes })
    }
}

/// Milliseconds since the Unix epoch (the id of a load's repairs).
pub fn now_ms() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(1, |d| d.as_millis() as u64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_new_load_gets_a_new_id_and_a_clean_one_clears_it() {
        let mut r = ConfigRepairs::default();
        r.loaded(vec!["a".into()], 5);
        assert_eq!(r.id, 5);
        r.loaded(vec!["b".into()], 5);
        assert_eq!((r.id, r.notes.clone()), (6, vec!["b".to_string()]), "same millisecond still moves the id");
        r.loaded(Vec::new(), 9);
        assert_eq!(r, ConfigRepairs::default());
    }
}
