//! Minimal rolling file log + stderr mirror.
//!
//! Writes to `%APPDATA%\recall-hdr\recall-hdr.log`, capped at [`MAX_LOG_BYTES`] at all times
//! (audit G13). When a controller takes the single-instance lock ([`new_run`]) the previous
//! run's log moves to `recall-hdr.log.1` (replacing the older one), so the lines that explain
//! a crash survive the restart. Only that controller rolls the log: a second start or a
//! `--probe` appends to the live file below the cap and never renames it. A run that reaches
//! the cap moves its own log to `.1` the same way and starts a fresh file, so the two files
//! never hold more than twice the cap; a rename that fails (another program holds the log)
//! is retried and, past twice the cap, the live file is emptied instead. Every line is also
//! mirrored to stderr so `--probe` / `--clamp-test` runs are readable straight from a console.
//!
//! The logger is process-global and lock-guarded; call `init()` once from `main`.

use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

/// The live log never grows past this; the previous file (`.1`) holds at most as much.
const MAX_LOG_BYTES: u64 = 1 << 20;

/// An append-only file that rolls to `<name>.1` before a line would take it past `cap`.
/// Only the log's owner rolls it (`may_roll`, set by [`new_run`] / [`own_log`]): another
/// recall-hdr.exe (a second start, `--probe`) appends below the cap and then writes to
/// stderr only, so it never renames the file under the controller (R2-deep-startup-6).
struct RollingFile {
    path: PathBuf,
    cap: u64,
    file: Option<File>,
    len: u64,
    may_roll: bool,
    /// After a failed rename: the length at which to try again (a reader holding the log
    /// without FILE_SHARE_DELETE makes every rename fail).
    retry_at: Option<u64>,
    rename_failures: u32,
}

impl RollingFile {
    /// Open for appending (the file is kept as it is).
    fn open(path: PathBuf, cap: u64) -> Self {
        let mut rolling = RollingFile { path, cap, file: None, len: 0, may_roll: false, retry_at: None, rename_failures: 0 };
        rolling.reopen();
        rolling
    }

    fn reopen(&mut self) {
        self.file = OpenOptions::new().create(true).append(true).open(&self.path).ok();
        self.len = fs::metadata(&self.path).map(|m| m.len()).unwrap_or(0);
    }

    /// Move a non-empty file to `.1` (replacing it) and start a fresh one. `incoming` is the
    /// line about to be written. When the rename fails, the file grows on and the rename is
    /// tried again every `cap / 16` bytes; a file that would pass twice the cap is emptied
    /// instead, so the log stays bounded whatever holds it.
    fn roll(&mut self, incoming: u64) {
        if self.len == 0 {
            return;
        }
        self.file = None; // close before the rename
        let renamed = fs::rename(&self.path, previous(&self.path)).is_ok();
        self.reopen();
        if renamed {
            self.retry_at = None;
            return;
        }
        self.rename_failures += 1;
        let step = (self.cap / 16).max(1);
        if self.len + incoming.max(step) > 2 * self.cap {
            self.clear();
        } else {
            self.retry_at = Some(self.len + step);
        }
    }

    /// Empty the live file (the rename keeps failing) and say why in it.
    fn clear(&mut self) {
        self.file = None;
        let _ = OpenOptions::new().write(true).truncate(true).open(&self.path);
        self.reopen();
        self.retry_at = None;
        if self.len > self.cap {
            self.file = None; // not even that worked: stderr only from here on
            self.may_roll = false;
            return;
        }
        self.append("(log cleared: it could not be moved to .1 because another program holds it open)");
    }

    /// Append one line (a newline is added), rolling first when it would pass the cap.
    fn write_line(&mut self, line: &str) {
        let bytes = line.len() as u64 + 1;
        if self.len + bytes > self.cap {
            if !self.may_roll {
                return; // another process owns this log: the line goes to stderr only
            }
            if self.retry_at.is_none_or(|at| self.len + bytes > at) {
                self.roll(bytes);
            }
        }
        self.append(line);
    }

    fn append(&mut self, line: &str) {
        if let Some(f) = self.file.as_mut() {
            if writeln!(f, "{line}").and_then(|_| f.flush()).is_ok() {
                self.len += line.len() as u64 + 1;
            }
        }
    }
}

/// `recall-hdr.log` -> `recall-hdr.log.1`.
fn previous(path: &Path) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(".1");
    PathBuf::from(name)
}

struct Logger {
    file: Mutex<RollingFile>,
    path: PathBuf,
}

static LOGGER: OnceLock<Logger> = OnceLock::new();

/// Resolve `%APPDATA%\recall-hdr\recall-hdr.log`, creating the directory.
fn log_path() -> PathBuf {
    let base = crate::user_dirs::appdata().unwrap_or_else(std::env::temp_dir);
    let path = crate::user_dirs::checked(base.join("recall-hdr").join("recall-hdr.log"));
    if let Some(dir) = path.parent() {
        let _ = fs::create_dir_all(dir);
    }
    path
}

/// Initialize the global logger. Safe to call more than once (subsequent calls
/// are no-ops). Never panics -- if the file can't be opened we still mirror to
/// stderr so a broken log dir doesn't take down the probe.
pub fn init() {
    LOGGER.get_or_init(|| {
        let path = log_path();
        Logger { file: Mutex::new(RollingFile::open(path.clone(), MAX_LOG_BYTES)), path }
    });
    log(&format!("=== recall-hdr start (log: {}) ===", LOGGER.get().unwrap().path.display()));
}

/// This process is the one controller now: keep the previous run's log as `.1` and start a
/// fresh file (`main::single_instance`, after the lock is taken).
pub fn new_run(what: &str) {
    if let Some(logger) = LOGGER.get() {
        if let Ok(mut file) = logger.file.lock() {
            file.may_roll = true;
            file.roll(0);
        }
    }
    log(&format!("=== recall-hdr run: {what} (previous run's log: recall-hdr.log.1) ==="));
}

/// This process runs as the controller without the single-instance lock (the mutex could not
/// be made): it keeps the log under the cap by rolling it, but keeps the previous run's file.
pub fn own_log() {
    if let Some(logger) = LOGGER.get() {
        if let Ok(mut file) = logger.file.lock() {
            file.may_roll = true;
        }
    }
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
        if let Ok(mut file) = logger.file.lock() {
            file.write_line(&line);
        }
    }
}

/// `log!("fmt", args...)` convenience wrapper around [`log`].
#[macro_export]
macro_rules! log {
    ($($arg:tt)*) => { $crate::logging::log(&format!($($arg)*)) };
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("recall-hdr-log-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn a_long_run_stays_under_the_cap_and_rolls_to_dot_1() {
        let dir = temp_dir("cap");
        let path = dir.join("recall-hdr.log");
        let mut log = RollingFile::open(path.clone(), 1024);
        log.may_roll = true; // the controller (new_run)
        let line = "x".repeat(99); // 100 bytes with the newline
        for _ in 0..30 {
            log.write_line(&line);
        }
        let live = fs::metadata(&path).unwrap().len();
        let old = fs::metadata(previous(&path)).unwrap().len();
        assert!(live <= 1024, "live log {live} bytes");
        assert!(old <= 1024 && old > 0, ".1 holds the rolled part ({old} bytes)");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_restart_keeps_the_previous_runs_log_as_dot_1() {
        let dir = temp_dir("restart");
        let path = dir.join("recall-hdr.log");
        fs::write(&path, "crash evidence\n".repeat(100_000)).unwrap(); // past the cap
        fs::write(previous(&path), "older run\n").unwrap();
        let mut log = RollingFile::open(path.clone(), MAX_LOG_BYTES);
        log.may_roll = true;
        log.roll(0);
        log.write_line("new run");
        let kept = fs::read_to_string(previous(&path)).unwrap();
        assert!(kept.starts_with("crash evidence"), "the previous run is kept, not deleted");
        assert_eq!(fs::read_to_string(&path).unwrap(), "new run\n");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_process_that_does_not_own_the_log_never_rolls_it() {
        // A second start or `--probe` next to the controller (R2-deep-startup-6): it appends
        // up to the cap, then writes to stderr only. The controller's file is never renamed.
        let dir = temp_dir("guest");
        let path = dir.join("recall-hdr.log");
        let mut log = RollingFile::open(path.clone(), 1024);
        let line = "x".repeat(99);
        for _ in 0..30 {
            log.write_line(&line);
        }
        assert!(!previous(&path).exists(), "only the log's owner rolls it");
        assert!(fs::metadata(&path).unwrap().len() <= 1024);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_log_held_open_by_a_reader_stays_bounded() {
        // A tail tool that opened the log without FILE_SHARE_DELETE makes every rename fail.
        use std::os::windows::fs::OpenOptionsExt;
        const SHARE_READ_WRITE: u32 = 0x1 | 0x2;
        let dir = temp_dir("held");
        let path = dir.join("recall-hdr.log");
        fs::write(&path, "").unwrap();
        let reader = OpenOptions::new().read(true).share_mode(SHARE_READ_WRITE).open(&path).unwrap();
        let mut log = RollingFile::open(path.clone(), 1024);
        log.may_roll = true;
        let line = "x".repeat(9); // 10 bytes with the newline
        for _ in 0..1000 {
            log.write_line(&line);
        }
        let live = fs::metadata(&path).unwrap().len();
        assert!(live <= 2048, "live log {live} bytes, at most twice the cap");
        // ~900 lines land past the cap; a rename is tried every cap/16 bytes, not per line.
        assert!(log.rename_failures < 250, "{} rename attempts for 1000 lines", log.rename_failures);
        drop(reader);
        let _ = fs::remove_dir_all(&dir);
    }
}
