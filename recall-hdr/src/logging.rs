//! Minimal rolling file log + stderr mirror.
//!
//! Writes to `%APPDATA%\recall-hdr\recall-hdr.log`. "Rolling" here is deliberately
//! cheap: on init, if the file is already past a size cap we truncate it (keep the
//! newest run, not an archive) -- this is a tray utility, not a service, so we
//! don't need multi-file rotation. Every line is also mirrored to stderr so
//! `--probe` / `--clamp-test` runs are readable straight from a console.
//!
//! The logger is process-global and lock-guarded; call `init()` once from `main`.

use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

/// Truncate the log on init once it grows past this. One run's worth of lines is
/// tiny; this only guards against an unbounded file across many runs.
const MAX_LOG_BYTES: u64 = 1_000_000;

struct Logger {
    file: Mutex<Option<File>>,
    path: PathBuf,
}

static LOGGER: OnceLock<Logger> = OnceLock::new();

/// Resolve `%APPDATA%\recall-hdr\recall-hdr.log`, creating the directory.
fn log_path() -> PathBuf {
    let base = std::env::var("APPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(|_| std::env::temp_dir());
    let dir = base.join("recall-hdr");
    let _ = fs::create_dir_all(&dir);
    dir.join("recall-hdr.log")
}

/// Initialize the global logger. Safe to call more than once (subsequent calls
/// are no-ops). Never panics -- if the file can't be opened we still mirror to
/// stderr so a broken log dir doesn't take down the probe.
pub fn init() {
    LOGGER.get_or_init(|| {
        let path = log_path();

        // Roll: drop the old file if it's grown past the cap.
        if let Ok(meta) = fs::metadata(&path) {
            if meta.len() > MAX_LOG_BYTES {
                let _ = fs::remove_file(&path);
            }
        }

        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .ok();

        Logger {
            file: Mutex::new(file),
            path,
        }
    });
    log(&format!("=== recall-hdr start (log: {}) ===", LOGGER.get().unwrap().path.display()));
}

/// Millisecond wall-clock since the epoch -- good enough to order lines without
/// pulling in a datetime dependency.
fn stamp() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0)
}

/// Append one line to the log file and mirror it to stderr. If `init()` was
/// never called, this still prints to stderr.
pub fn log(msg: &str) {
    let line = format!("[{}] {}", stamp(), msg);
    eprintln!("{line}");
    if let Some(logger) = LOGGER.get() {
        if let Ok(mut guard) = logger.file.lock() {
            if let Some(f) = guard.as_mut() {
                let _ = writeln!(f, "{line}");
                let _ = f.flush();
            }
        }
    }
}

/// `log!("fmt", args...)` convenience wrapper around [`log`].
#[macro_export]
macro_rules! log {
    ($($arg:tt)*) => { $crate::logging::log(&format!($($arg)*)) };
}
