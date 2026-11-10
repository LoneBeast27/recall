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

/// Where an MP4's media can start: after its header boxes (`ftyp`, `free`, `wide`, `skip`,
/// `moov`, `uuid`), at the payload of its first `mdat`, or at anything else (a `moof`, a torn or
/// unknown box). The file's length when it holds only header boxes.
fn media_start(file: &mut std::fs::File, len: u64) -> u64 {
    let mut pos = 0u64;
    while pos + 8 <= len {
        let mut head = [0u8; 16];
        if file.seek(SeekFrom::Start(pos)).and_then(|_| file.read_exact(&mut head[..8])).is_err() {
            return pos;
        }
        let size32 = u32::from_be_bytes([head[0], head[1], head[2], head[3]]) as u64;
        let large = size32 == 1 && pos + 16 <= len && file.read_exact(&mut head[8..16]).is_ok();
        let (header, size) = if large { (16, u64::from_be_bytes(head[8..16].try_into().unwrap_or_default())) } else { (8, size32) };
        match &head[4..8] {
            b"mdat" => return (pos + header).min(len),
            b"ftyp" | b"free" | b"wide" | b"skip" | b"moov" | b"uuid" if size >= header && pos + size <= len => pos += size,
            _ => return pos,
        }
    }
    len
}

/// Where the data in a file of `len` bytes ends: before a trailing run of zero bytes.
pub(crate) fn data_end(file: &mut std::fs::File, len: u64) -> u64 {
    let (mut end, mut buf) = (len, vec![0u8; 64 * 1024]);
    while end > 0 {
        let n = buf.len().min(end as usize);
        if file.seek(SeekFrom::Start(end - n as u64)).and_then(|_| file.read_exact(&mut buf[..n])).is_err() {
            return len;
        }
        if let Some(last) = buf[..n].iter().rposition(|b| *b != 0) {
            return end - n as u64 + last as u64 + 1;
        }
        end -= n as u64;
    }
    0
}

/// Whether the file itself holds media data: any non-zero byte past its header boxes. Unreadable
/// counts as holding media (never recycled on a guess).
pub(crate) fn holds_media(path: &Path) -> bool {
    let Ok(mut file) = std::fs::File::open(path) else { return true };
    let len = file.metadata().map(|m| m.len()).unwrap_or(u64::MAX);
    let start = media_start(&mut file, len);
    data_end(&mut file, len) > start
}

/// `path` holds no decodable packet by its index or its demuxer (`why`): one diagnostic line,
/// then the Recycle Bin, but only when a scan of the file itself finds no media data in it
/// ([`holds_media`]); a file with data in it is kept (the same one line says so). The lines to
/// log (a second says where the file went, or why it stayed).
pub(crate) fn dispose_unrecoverable(path: &Path, why: &str) -> Vec<String> {
    let size = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
    if holds_media(path) {
        return vec![format!("unrecoverable {}: {size} B, boxes {}, why={why}; kept: the file holds media data", path.display(), layout_summary(path))];
    }
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
