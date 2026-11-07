//! Make a finished move durable on the clip folder's volume before the staged copy on the
//! other volume is deleted or kept (`mover.rs`). The two volumes keep separate NTFS logs, so
//! nothing orders them: without this, a power cut a few seconds after a move could keep the
//! C: side (staged copy deleted, or kept and adopted by the app) while V: rolled back its
//! `.part` -> `.mp4` rename, leaving the clip only as a hidden `.part`.
//!
//! [`flush_dest`]: `FlushFileBuffers` on the moved clip and on its folder (opened with
//! `FILE_FLAG_BACKUP_SEMANTICS`), which commits the rename's log records. A file system that
//! has no directory flush (`ERROR_INVALID_FUNCTION` / `ERROR_NOT_SUPPORTED`, e.g. FAT) counts
//! as flushed once the file is; any other failure keeps the staged copy (`mover.rs`).

use std::fs::OpenOptions;
use std::os::windows::fs::OpenOptionsExt;
use std::path::Path;

const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;
const ERROR_INVALID_FUNCTION: i32 = 1;
const ERROR_NOT_SUPPORTED: i32 = 50;

/// `fn(moved clip) -> flushed`: [`flush_dest`], or a failing one in tests.
pub(crate) type FlushFn = fn(&Path) -> std::io::Result<()>;

/// Flush the clip at `dest` and the folder that names it (see the module doc).
pub(crate) fn flush_dest(dest: &Path) -> std::io::Result<()> {
    OpenOptions::new().write(true).open(dest)?.sync_all()?;
    let Some(dir) = dest.parent().filter(|d| !d.as_os_str().is_empty()) else { return Ok(()) };
    let flushed = OpenOptions::new().write(true).custom_flags(FILE_FLAG_BACKUP_SEMANTICS).open(dir).and_then(|d| d.sync_all());
    match flushed {
        Err(e) if matches!(e.raw_os_error(), Some(ERROR_INVALID_FUNCTION | ERROR_NOT_SUPPORTED)) => Ok(()),
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_moved_clip_and_its_folder_flush_on_this_machine() {
        let dir = std::env::temp_dir().join(format!("recall-flush-{}", uuid::Uuid::new_v4().simple()));
        std::fs::create_dir_all(&dir).unwrap();
        let clip = dir.join("G.mp4");
        std::fs::write(&clip, b"clip").unwrap();
        assert!(flush_dest(&clip).is_ok(), "{:?}", flush_dest(&clip));
        assert!(flush_dest(&dir.join("missing.mp4")).is_err(), "a missing clip is not flushed");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
