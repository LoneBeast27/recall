//! Island sizes set with the editor's panel, per exe stem, in their own file
//! `%APPDATA%\Recall\island-sizes.json`. Not in `island-hud.json`: older
//! builds rewrite that file whole from their own fields, so a rollback would
//! drop the sizes on its first save. No older build knows this file.
//!
//! 100% is never written. Read entry by entry like `dynamic_file.rs`: an
//! entry this build can't use drops only itself (the file is copied to a
//! dated `.bad` first); a file that is not a JSON object is moved there; a
//! file that can't be read leaves the store read-only for the run.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::durable::{self, Keep};
use crate::dynamic::{exe_stem_key, MAX_GAMES};
use crate::panel::{sane_size, size_pct};

#[derive(Debug, Default, PartialEq, Serialize, Deserialize)]
struct SizeFile {
    #[serde(default)]
    seq: u64,
    #[serde(default)]
    sizes: BTreeMap<String, f32>,
    /// When each size was last set (`seq` then): the oldest go past [`MAX_GAMES`].
    #[serde(default)]
    sizes_seq: BTreeMap<String, u64>,
}

#[derive(Debug, Default)]
pub struct SizeStore {
    path: Option<PathBuf>,
    file: SizeFile,
    read_only: bool,
}

impl SizeStore {
    /// `%APPDATA%\Recall\island-sizes.json`.
    pub fn default_path() -> Option<PathBuf> {
        crate::user_dirs::appdata().map(|dir| crate::user_dirs::checked(dir.join("Recall").join("island-sizes.json")))
    }

    pub fn load(path: Option<PathBuf>) -> Self {
        let (file, writable) = match path.as_ref() {
            Some(p) => read(p, SystemTime::now()),
            None => (SizeFile::default(), true),
        };
        Self { path, file, read_only: !writable }
    }

    /// The island size the user set for the game (1.0 when none).
    pub fn size(&self, key: &str) -> f32 {
        self.file.sizes.get(key).copied().unwrap_or(1.0)
    }

    /// Keeps the game's island size and saves; 100% (or a size out of
    /// range) clears it. Only an exe-stem key is kept (never a window title).
    pub fn set_size(&mut self, key: &str, size: f32) {
        if !exe_stem_key(key) {
            return;
        }
        let file = &mut self.file;
        if sane_size(size) && size_pct(size) != 100 {
            file.seq += 1;
            file.sizes.insert(key.to_string(), size);
            file.sizes_seq.insert(key.to_string(), file.seq);
        } else {
            file.sizes.remove(key);
            file.sizes_seq.remove(key);
        }
        file.sizes_seq.retain(|key, _| file.sizes.contains_key(key));
        while file.sizes.len() > MAX_GAMES {
            let Some(oldest) = file.sizes.keys().min_by_key(|k| file.sizes_seq.get(*k).copied().unwrap_or(0)).cloned() else { break };
            file.sizes.remove(&oldest);
            file.sizes_seq.remove(&oldest);
        }
        self.save();
    }

    /// Written whole to a temp file, then renamed over; never over a file
    /// this run could not read.
    fn save(&self) {
        let Some(path) = self.path.as_ref().filter(|_| !self.read_only) else { return };
        let Ok(text) = serde_json::to_string(&self.file) else { return };
        let _ = durable::write_durable(path, text.as_bytes());
    }
}

fn read(path: &Path, now: SystemTime) -> (SizeFile, bool) {
    durable::sweep_temps(path, now);
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return (SizeFile::default(), true),
        Err(_) => return (SizeFile::default(), false),
    };
    let Ok(Value::Object(root)) = serde_json::from_str::<Value>(&text) else {
        durable::quarantine(path, now, Keep::Move);
        return (SizeFile::default(), true);
    };
    let mut lossy = false;
    let mut entries = |key: &str| -> BTreeMap<String, Value> {
        match root.get(key) {
            None => BTreeMap::new(),
            Some(Value::Object(map)) => map.clone().into_iter().collect(),
            Some(_) => {
                lossy = true;
                BTreeMap::new()
            }
        }
    };
    let (sizes, seqs) = (entries("sizes"), entries("sizes_seq"));
    let mut file = SizeFile { seq: root.get("seq").and_then(Value::as_u64).unwrap_or(0), ..SizeFile::default() };
    for (key, value) in sizes {
        match value.as_f64().map(|v| v as f32).filter(|&v| sane_size(v) && exe_stem_key(&key)) {
            Some(size) => {
                file.sizes.insert(key, size);
            }
            None => lossy = true,
        }
    }
    file.sizes_seq = seqs.into_iter().filter_map(|(k, v)| Some((k, v.as_u64()?))).collect();
    if lossy {
        durable::quarantine(path, now, Keep::Copy);
    }
    (file, true)
}

#[cfg(test)]
#[path = "island_sizes_tests.rs"]
mod tests;
