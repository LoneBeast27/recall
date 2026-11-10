//! Never a permanent delete: an output file Recall wrote that holds nothing playable goes to the
//! Recycle Bin, and only after one diagnostic log line that says what was in it and why it
//! could not be recovered (so a recovery bug can be found from the log).
//!
//! - [`recycle`]: `win_recycle`'s precheck (the volume has a bin that will take the file), then
//!   `win_fileop`'s `IFileOperation` with the sink that refuses any permanent delete. On a volume
//!   without a bin, or any refusal, the file stays where it is.
//! - Unit tests never reach the real Recycle Bin: under `cfg(test)` [`recycle`] is [`fake`],
//!   which moves the file into `.recall-test-bin\` beside it (inside the test's own temp folder).
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

/// Move `path` to the Recycle Bin. `Err` (why): the file stays where it is.
pub(crate) fn recycle(path: &Path) -> Result<(), String> {
    #[cfg(test)]
    return fake::recycle(path);
    #[allow(unreachable_code)]
    real(path)
}

#[cfg_attr(test, allow(dead_code))]
fn real(path: &Path) -> Result<(), String> {
    precheck(path)?;
    outcome(crate::win_fileop::recycle_files(&[path.to_path_buf()]).outcome)
}

/// `win_recycle`'s check that `path`'s volume has a Recycle Bin that will take it (reads the
/// volume and the bin's settings; moves nothing).
pub(crate) fn precheck(path: &Path) -> Result<(), String> {
    let bytes = std::fs::metadata(path).map_err(|e| format!("read {}: {e}", path.display()))?.len();
    let dir = path.parent().filter(|d| !d.as_os_str().is_empty()).unwrap_or(Path::new("."));
    crate::win_recycle::can_recycle(dir, bytes)
}

/// What `win_fileop`'s operation did, as this module's answer: only `Recycled` is done.
pub(crate) fn outcome(outcome: crate::win_fileop::Outcome) -> Result<(), String> {
    match outcome {
        crate::win_fileop::Outcome::Recycled => Ok(()),
        crate::win_fileop::Outcome::Refused(why) | crate::win_fileop::Outcome::Failed(why) => Err(why),
    }
}

/// The top-level boxes of `path` from their headers only: `[ftyp 32, free 8, mdat 8+1204]`,
/// a torn or unknown tail as `?? <n> B`, an all-zero tail as `zeros <n> B`.
pub(crate) fn layout_summary(path: &Path) -> String {
    let Ok(mut file) = std::fs::File::open(path) else { return "[unreadable]".into() };
    let len = file.metadata().map(|m| m.len()).unwrap_or(0);
    let (mut pos, mut parts) = (0u64, Vec::new());
    while pos + 8 <= len && parts.len() < 12 {
        let mut head = [0u8; 8];
        if file.seek(SeekFrom::Start(pos)).and_then(|_| file.read_exact(&mut head)).is_err() {
            break;
        }
        let size = u32::from_be_bytes([head[0], head[1], head[2], head[3]]) as u64;
        let kind = &head[4..8];
        if head == [0u8; 8] {
            parts.push(format!("zeros {} B", len - pos));
            pos = len;
            break;
        }
        if !kind.iter().all(u8::is_ascii_alphanumeric) || size < 8 || pos + size > len {
            break;
        }
        parts.push(format!("{} {}", String::from_utf8_lossy(kind), size));
        pos += size;
    }
    if pos < len {
        parts.push(format!("?? {} B", len - pos));
    }
    format!("[{}]", parts.join(", "))
}

/// `path` holds no decodable packet (`why`): one diagnostic line, then the Recycle Bin. The
/// lines to log (the second says where the file went, or why it stayed).
pub(crate) fn dispose_unrecoverable(path: &Path, why: &str) -> Vec<String> {
    let size = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
    let mut lines = vec![format!("unrecoverable {}: {size} B, boxes {}, why={why}", path.display(), layout_summary(path))];
    lines.push(match recycle(path) {
        Ok(()) => format!("moved {} to the Recycle Bin", path.display()),
        Err(e) => format!("{} left where it is: {e}", path.display()),
    });
    lines
}

/// Tests: the Recycle Bin, faked (see the module doc).
#[cfg(test)]
pub(crate) mod fake {
    use super::*;
    use std::cell::Cell;
    use std::path::PathBuf;
    use std::sync::Mutex;

    static BINNED: Mutex<Vec<PathBuf>> = Mutex::new(Vec::new());
    thread_local! { static NO_BIN: Cell<bool> = const { Cell::new(false) }; }

    pub(crate) fn recycle(path: &Path) -> Result<(), String> {
        if NO_BIN.with(Cell::get) {
            return Err("no Recycle Bin on this volume (test)".into());
        }
        let bin = path.parent().unwrap_or(Path::new(".")).join(".recall-test-bin");
        std::fs::create_dir_all(&bin).map_err(|e| e.to_string())?;
        let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let to = bin.join(format!("{}-{name}", uuid::Uuid::new_v4().simple()));
        std::fs::rename(path, &to).map_err(|e| format!("fake bin: {e}"))?;
        BINNED.lock().unwrap_or_else(|p| p.into_inner()).push(path.to_path_buf());
        Ok(())
    }

    /// Every file sent to the (fake) Recycle Bin from under `dir`, by its original path.
    pub(crate) fn binned_under(dir: &Path) -> Vec<PathBuf> {
        BINNED.lock().unwrap_or_else(|p| p.into_inner()).iter().filter(|p| p.starts_with(dir)).cloned().collect()
    }

    /// Run `f` as on a volume without a Recycle Bin (this thread only).
    pub(crate) fn without_bin<R>(f: impl FnOnce() -> R) -> R {
        NO_BIN.with(|b| b.set(true));
        let out = f();
        NO_BIN.with(|b| b.set(false));
        out
    }
}
