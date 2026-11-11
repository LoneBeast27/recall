//! An append-only trace file that moves to `<name>.1` before a line would take it past its
//! cap, so a trace never holds more than twice the cap on disk.

use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

pub struct RollingFile {
    path: PathBuf,
    cap: u64,
    file: Option<File>,
    len: u64,
}

/// `recorder.trace` -> `recorder.trace.1`.
pub fn previous(path: &Path) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(".1");
    PathBuf::from(name)
}

impl RollingFile {
    pub fn open(path: PathBuf, cap: u64) -> Self {
        let mut rolling = RollingFile { path, cap, file: None, len: 0 };
        rolling.reopen();
        rolling
    }

    fn reopen(&mut self) {
        if let Some(dir) = self.path.parent() {
            let _ = fs::create_dir_all(dir);
        }
        self.file = OpenOptions::new().create(true).append(true).open(&self.path).ok();
        self.len = fs::metadata(&self.path).map(|m| m.len()).unwrap_or(0);
    }

    /// Append one line (a newline is added). A failed write is dropped: the trace never
    /// stops the program.
    pub fn write_line(&mut self, line: &str) {
        let bytes = line.len() as u64 + 1;
        if self.len > 0 && self.len + bytes > self.cap {
            self.file = None;
            if fs::rename(&self.path, previous(&self.path)).is_err() {
                // Another program holds it: start over in place rather than grow.
                let _ = OpenOptions::new().write(true).truncate(true).open(&self.path);
            }
            self.reopen();
        }
        if let Some(f) = self.file.as_mut() {
            if writeln!(f, "{line}").and_then(|_| f.flush()).is_ok() {
                self.len += bytes;
            }
        }
    }
}
