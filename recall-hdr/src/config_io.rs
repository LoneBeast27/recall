//! config.toml load/save IO (deep-audit S3, S6, S11).
//!
//! - A file that parses is remembered as `config.toml.last-good`.
//! - A wrongly typed value costs only its own key: the rest is salvaged key by
//!   key over the defaults, and the original is copied to a dated `.bad`.
//! - A file that is not TOML (NUL-filled after a power cut, garbage) is moved to
//!   a dated `.bad`; the last good load is used, else the defaults. An empty file
//!   (a power cut before the data reached the disk) also gets the last good load.
//! - A save keeps the keys this build doesn't know (a newer build's, a hand-added
//!   section) from the file it replaces, and is written with `sync_all`.
use std::fs;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use toml::{Table, Value};

use super::Config;
use crate::durable::{self, Keep};
use crate::log;

/// Top-level tables an older build wrote and this one reads only to migrate:
/// never carried forward (the chord-era `[hotkeys]` would re-run its migration).
const RETIRED: &[&str] = &["hotkeys"];
/// Schema keys absent from the serialized form when unset.
const OPTIONAL: &[(&str, &str)] = &[("display", "target_device_path")];

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

fn remember_good(path: &Path, bytes: &[u8]) {
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

/// The defaults with every key of `disk` that parses on its own laid over them.
fn salvage(disk: &Table) -> Config {
    let mut merged = defaults_table();
    for (key, value) in disk {
        match (value, merged.get(key)) {
            (Value::Table(inner), Some(Value::Table(_))) => {
                for (leaf, v) in inner {
                    try_set(&mut merged, Some(key), leaf, v);
                }
            }
            _ => try_set(&mut merged, None, key, value),
        }
    }
    Value::Table(merged).try_into().unwrap_or_default()
}

fn try_set(merged: &mut Table, section: Option<&str>, key: &str, value: &Value) {
    let mut trial = merged.clone();
    let slot = match section {
        Some(s) => match trial.get_mut(s) {
            Some(Value::Table(t)) => t,
            _ => return,
        },
        None => &mut trial,
    };
    slot.insert(key.to_string(), value.clone());
    if Value::Table(trial.clone()).try_into::<Config>().is_ok() {
        *merged = trial;
    } else {
        let name = section.map_or(key.to_string(), |s| format!("{s}.{key}"));
        log!("config: {name} has a bad value; reset to the default");
    }
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
