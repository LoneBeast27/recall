//! The island's change-only log (`%APPDATA%\recall-hdr\recall-overlay.log`):
//! one line per value that changed, never one per frame. Each key keeps its
//! last value; a repeat only bumps a counter that the next change reports.
//! The file moves to `.old` once it would pass `limit` bytes (or starts over
//! in place when it can't be moved).

use std::collections::HashMap;
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

/// The log's size cap before it rotates to `.old`.
pub const LIMIT: u64 = 1024 * 1024;

#[derive(Default)]
pub struct Trace {
    path: Option<PathBuf>,
    file: Option<File>,
    size: u64,
    limit: u64,
    /// Per key: the last value written and how often it repeated since.
    last: HashMap<&'static str, (String, u64)>,
}

impl Trace {
    /// The live log under `%APPDATA%\recall-hdr` (none without APPDATA).
    pub fn default_path() -> Option<PathBuf> {
        std::env::var_os("APPDATA").map(|dir| PathBuf::from(dir).join("recall-hdr").join("recall-overlay.log"))
    }

    /// A log at `path` (none: every call is a no-op) rotating past `limit` bytes.
    pub fn open(path: Option<PathBuf>, limit: u64) -> Self {
        Self { path, limit, ..Self::default() }
    }

    /// True when lines go to a file (live runs; smoke runs keep none).
    pub fn enabled(&self) -> bool {
        self.path.is_some()
    }

    /// Writes `key: value` when `value` differs from the key's last one; a
    /// change after repeats says how many calls the old value held for.
    /// Returns true when a line was written.
    pub fn note(&mut self, key: &'static str, value: impl Into<String>) -> bool {
        let value = value.into();
        match self.last.get_mut(key) {
            Some((old, repeats)) if *old == value => {
                *repeats += 1;
                return false;
            }
            _ => {}
        }
        let held = self.last.get(key).map_or(0, |(_, repeats)| *repeats);
        let line = if held > 0 { format!("{key}: {value} (previous held {} more calls)", held) } else { format!("{key}: {value}") };
        self.last.insert(key, (value, 0));
        self.write(&line)
    }

    /// Writes a one-off line (start, a failure) without the change check.
    pub fn event(&mut self, line: &str) -> bool {
        self.write(line)
    }

    fn write(&mut self, line: &str) -> bool {
        let Some(path) = self.path.clone() else { return false };
        let ms = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_millis());
        let text = format!("[{ms}] {line}\n");
        if self.file.is_none() {
            self.file = open_append(&path);
            self.size = fs::metadata(&path).map_or(0, |m| m.len());
        }
        if self.size > 0 && self.size + text.len() as u64 > self.limit {
            self.file = None;
            // A log held open without delete sharing can't move: start it over in
            // place; if even that fails, write nothing until it can rotate.
            self.file = match fs::rename(&path, old_path(&path)) {
                Ok(()) => open_append(&path),
                Err(_) => truncate(&path),
            };
            if self.file.is_some() {
                self.size = 0;
            }
        }
        let Some(file) = self.file.as_mut() else { return false };
        if file.write_all(text.as_bytes()).is_err() {
            self.file = None;
            return false;
        }
        self.size += text.len() as u64;
        true
    }
}

fn open_append(path: &Path) -> Option<File> {
    if let Some(dir) = path.parent() {
        let _ = fs::create_dir_all(dir);
    }
    OpenOptions::new().create(true).append(true).open(path).ok()
}

/// The log emptied in place, opened for appending.
fn truncate(path: &Path) -> Option<File> {
    OpenOptions::new().write(true).truncate(true).open(path).ok()?;
    open_append(path)
}

/// `recall-overlay.log` → `recall-overlay.log.old`.
pub fn old_path(path: &Path) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(".old");
    PathBuf::from(name)
}

#[cfg(test)]
#[path = "trace_tests.rs"]
mod tests;
