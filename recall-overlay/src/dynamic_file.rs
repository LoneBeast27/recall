//! Reading `island-hud.json` (deep-audit O3): entry by entry, so one entry this
//! build can't read (a newer build's edge, a hand edit) drops only itself.
//! A file that is not a JSON object is moved to a dated `.bad`; a file read with
//! losses is copied there first. A file that can't be read (an antivirus scan
//! holding it) leaves the cache read-only for the run, so it is never replaced.
use std::collections::BTreeMap;
use std::path::Path;
use std::time::SystemTime;

use serde::de::DeserializeOwned;
use serde_json::{Map, Value};

use super::CacheFile;
use crate::durable::{self, Keep};

/// What loading found: the cache, and whether saving over the file is allowed.
pub(super) fn read(path: &Path, now: SystemTime) -> (CacheFile, bool) {
    durable::sweep_temps(path, now);
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return (CacheFile::default(), true),
        Err(_) => return (CacheFile::default(), false),
    };
    match parse(&text) {
        Some((file, false)) => (file, true),
        Some((file, true)) => {
            durable::quarantine(path, now, Keep::Copy);
            (file, true)
        }
        None => {
            durable::quarantine(path, now, Keep::Move);
            (CacheFile::default(), true)
        }
    }
}

/// The cache and whether anything was dropped; None when `text` is not a JSON object.
fn parse(text: &str) -> Option<(CacheFile, bool)> {
    let Ok(Value::Object(root)) = serde_json::from_str::<Value>(text) else { return None };
    let mut lossy = false;
    let file = CacheFile {
        games: entries(&root, "games", &mut lossy),
        seq: field(&root, "seq", &mut lossy).unwrap_or(0),
        spots: entries(&root, "spots", &mut lossy),
        spots_seq: entries(&root, "spots_seq", &mut lossy),
    };
    Some((file, lossy))
}

fn field<T: DeserializeOwned>(root: &Map<String, Value>, key: &str, lossy: &mut bool) -> Option<T> {
    let value = root.get(key)?;
    let parsed = serde_json::from_value(value.clone()).ok();
    *lossy |= parsed.is_none();
    parsed
}

fn entries<T: DeserializeOwned>(root: &Map<String, Value>, key: &str, lossy: &mut bool) -> BTreeMap<String, T> {
    let Some(value) = root.get(key) else { return BTreeMap::new() };
    let Value::Object(map) = value else {
        *lossy = true;
        return BTreeMap::new();
    };
    let mut out = BTreeMap::new();
    for (name, entry) in map {
        match serde_json::from_value(entry.clone()) {
            Ok(parsed) => {
                out.insert(name.clone(), parsed);
            }
            Err(_) => *lossy = true,
        }
    }
    out
}
