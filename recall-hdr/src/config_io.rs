//! config.toml load/save IO (deep-audit S3, S6, S11).
//!
//! - A file that parses, and every save, is remembered as `config.toml.last-good`.
//! - A file that exists but can't be read (a backup tool holding it) is retried
//!   briefly; then the caller gets an error and must not save over it.
//! - A wrongly typed value costs only its own key (or its own element of an
//!   array: one bad audio source drops only itself): the rest is salvaged over
//!   the defaults, and the original is copied to a dated `.bad`.
//! - A file that is not TOML (NUL-filled after a power cut, garbage) is moved to
//!   a dated `.bad`; the last good load is used, else the defaults. An empty file
//!   (a power cut before the data reached the disk) also gets the last good load.
//! - A save keeps the keys this build doesn't know (a newer build's, a hand-added
//!   section) from the file it replaces, and is written with `sync_all`.
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use toml::{Table, Value};

use super::Config;
use crate::durable::{self, Keep};
use crate::log;

/// Top-level tables an older build wrote and this one reads only to migrate:
/// never carried forward (the chord-era `[hotkeys]` would re-run its migration).
const RETIRED: &[&str] = &["hotkeys"];
/// Schema keys absent from the serialized form when unset.
const OPTIONAL: &[(&str, &str)] = &[("display", "target_device_path")];

/// Reads of a file that exists but can't be read, and the pause between them.
const READ_TRIES: u32 = 3;
const READ_PAUSE: Duration = Duration::from_millis(200);

/// The bytes at `path`, None when it is missing. A failed read (locked, denied) is
/// retried [`READ_TRIES`] times, `pause` apart.
pub(super) fn read_retrying(path: &Path, pause: &dyn Fn(Duration)) -> std::io::Result<Option<Vec<u8>>> {
    let mut tries = 1;
    loop {
        match fs::read(path) {
            Ok(bytes) => return Ok(Some(bytes)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) if tries >= READ_TRIES => return Err(e),
            Err(e) => {
                log!("config: read {} failed ({e}); trying again", path.display());
                tries += 1;
                pause(READ_PAUSE);
            }
        }
    }
}

/// `config.toml.last-good`.
pub(super) fn last_good_path(path: &Path) -> PathBuf {
    let mut name = path.file_name().map(|n| n.to_os_string()).unwrap_or_default();
    name.push(".last-good");
    path.with_file_name(name)
}

/// The config in `bytes` (read from `path`) and the text its migrations read;
/// None = use the defaults.
pub(super) fn parse_or_recover(path: &Path, bytes: &[u8], now: SystemTime) -> Option<(Config, String)> {
    let Ok(text) = std::str::from_utf8(bytes) else {
        log!("config: {} is not text; backing it up", path.display());
        durable::quarantine(path, now, Keep::Move);
        return last_good(path);
    };
    if text.trim().is_empty() {
        log!("config: {} is empty; using the last good config", path.display());
        return last_good(path);
    }
    match toml::from_str::<Config>(text) {
        Ok(cfg) => {
            remember_good(path, bytes);
            Some((cfg, text.to_string()))
        }
        Err(e) => match toml::from_str::<Table>(text) {
            Ok(table) => {
                log!("config: {} has a bad value ({e}); keeping the rest", path.display());
                // Copy: the file stays, so a save still carries its unknown keys.
                durable::quarantine(path, now, Keep::Copy);
                Some((salvage(&table), text.to_string()))
            }
            Err(_) => {
                log!("config: parse {} failed: {e}; backing it up", path.display());
                durable::quarantine(path, now, Keep::Move);
                last_good(path)
            }
        },
    }
}

fn last_good(path: &Path) -> Option<(Config, String)> {
    let text = fs::read_to_string(last_good_path(path)).ok()?;
    let cfg = toml::from_str::<Config>(&text).ok()?;
    log!("config: using {}", last_good_path(path).display());
    Some((cfg, text))
}

/// Keeps `bytes` (a config that parses) as `config.toml.last-good`.
pub(super) fn remember_good(path: &Path, bytes: &[u8]) {
    let good = last_good_path(path);
    if fs::read(&good).ok().as_deref() != Some(bytes) {
        if let Err(e) = durable::write_durable(&good, bytes) {
            log!("config: could not keep {}: {e}", good.display());
        }
    }
}

fn defaults_table() -> Table {
    match Value::try_from(Config::default()) {
        Ok(Value::Table(t)) => t,
        _ => Table::new(),
    }
}

/// The defaults with every part of `disk` that parses on its own laid over them.
fn salvage(disk: &Table) -> Config {
    let mut merged = defaults_table();
    for (key, value) in disk {
        lay(&mut merged, &[key.as_str()], value);
    }
    Value::Table(merged).try_into().unwrap_or_default()
}

/// Lays `value` at `at` when the config still parses with it; else, for a table, each
/// of its keys, and for an array, each element (a bad one drops only itself).
fn lay(merged: &mut Table, at: &[&str], value: &Value) {
    if try_set(merged, at, value.clone()) {
        return;
    }
    let name = at.join(".");
    match value {
        Value::Table(inner) if table_at(merged, at) => {
            for (leaf, v) in inner {
                let mut next = at.to_vec();
                next.push(leaf);
                lay(merged, &next, v);
            }
        }
        Value::Array(items) => {
            let mut kept: Vec<Value> = Vec::new();
            for (i, item) in items.iter().enumerate() {
                let mut trial = kept.clone();
                trial.push(item.clone());
                if try_set(merged, at, Value::Array(trial)) {
                    kept.push(item.clone());
                } else {
                    log!("config: {name}[{i}] has a bad value; dropped");
                }
            }
            if kept.is_empty() {
                log!("config: {name} has no readable entry; reset to the default");
            }
        }
        _ => log!("config: {name} has a bad value; reset to the default"),
    }
}

/// Whether `merged` holds a table at `at`.
fn table_at(merged: &Table, at: &[&str]) -> bool {
    let mut cur = merged;
    for key in at {
        match cur.get(*key) {
            Some(Value::Table(t)) => cur = t,
            _ => return false,
        }
    }
    true
}

/// Sets `merged[at] = value` when the config still parses with it.
fn try_set(merged: &mut Table, at: &[&str], value: Value) -> bool {
    let Some((last, parents)) = at.split_last() else { return false };
    let mut trial = merged.clone();
    let mut slot = &mut trial;
    for key in parents {
        slot = match slot.get_mut(*key) {
            Some(Value::Table(t)) => t,
            _ => return false,
        };
    }
    slot.insert((*last).to_string(), value);
    if Value::Table(trial.clone()).try_into::<Config>().is_err() {
        return false;
    }
    *merged = trial;
    true
}

/// Keys of `disk` (the file being replaced) this build's schema doesn't have.
fn unknown_keys(disk: &Table) -> Table {
    let known = defaults_table();
    let mut out = Table::new();
    for (key, value) in disk {
        if RETIRED.contains(&key.as_str()) {
            continue;
        }
        match (value, known.get(key)) {
            (_, None) => {
                out.insert(key.clone(), value.clone());
            }
            (Value::Table(inner), Some(Value::Table(schema))) => {
                let extra: Table = inner
                    .iter()
                    .filter(|(leaf, _)| !schema.contains_key(*leaf) && !OPTIONAL.contains(&(key.as_str(), leaf.as_str())))
                    .map(|(leaf, v)| (leaf.clone(), v.clone()))
                    .collect();
                if !extra.is_empty() {
                    out.insert(key.clone(), Value::Table(extra));
                }
            }
            _ => {}
        }
    }
    out
}

/// `cfg` as TOML, plus the unknown keys of the file at `path` it replaces.
pub(super) fn serialize_for(cfg: &Config, path: &Path) -> Result<String, String> {
    let text = toml::to_string_pretty(cfg).map_err(|e| format!("serialize config: {e}"))?;
    let disk = fs::read_to_string(path).ok().and_then(|t| toml::from_str::<Table>(&t).ok());
    let extra = disk.as_ref().map(unknown_keys).unwrap_or_default();
    if extra.is_empty() {
        return Ok(text);
    }
    let mut out: Table = toml::from_str(&text).map_err(|e| format!("reparse config: {e}"))?;
    for (key, value) in extra {
        match (out.get_mut(&key), value) {
            (Some(Value::Table(section)), Value::Table(leaves)) => section.extend(leaves),
            (_, value) => {
                out.insert(key, value);
            }
        }
    }
    toml::to_string_pretty(&out).map_err(|e| format!("serialize config: {e}"))
}
