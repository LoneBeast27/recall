//! Make a finished move durable on the clip folder's volume before the staged copy on the
//! other volume is deleted or kept (`mover.rs`). The two volumes keep separate NTFS logs, so
//! nothing orders them: without this, a power cut a few seconds after a move could keep the
//! C: side (staged copy deleted, or kept and adopted by the app) while V: rolled back its
//! `.part` -> `.mp4` rename, leaving the clip only as a hidden `.part`.
//!
//! [`flush_dest`]: `FlushFileBuffers` on the moved clip and on its folder (opened with
//! `FILE_FLAG_BACKUP_SEMANTICS`), which commits the rename's log records. A file system that
//! has no directory flush (`ERROR_INVALID_FUNCTION` / `ERROR_NOT_SUPPORTED`, e.g. FAT), or
//! refuses a folder handle for it (`ERROR_ACCESS_DENIED`: SMB / NAS shares, some ACLs), counts
//! as flushed once the file is: the file flush commits the volume's log up to that file, and a
//! share flushes on its server. Any other failure keeps the staged copy and is retried
//! (`mover.rs`, `mover_retry.rs`).

use std::fs::OpenOptions;
use std::os::windows::fs::OpenOptionsExt;
use std::path::Path;

const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;
const ERROR_INVALID_FUNCTION: i32 = 1;
const ERROR_ACCESS_DENIED: i32 = 5;
const ERROR_NOT_SUPPORTED: i32 = 50;

/// `fn(moved clip) -> flushed`: [`flush_dest`], or a failing one in tests.
pub(crate) type FlushFn = fn(&Path) -> std::io::Result<()>;

/// Flush the clip at `dest` and the folder that names it (see the module doc).
pub(crate) fn flush_dest(dest: &Path) -> std::io::Result<()> {
    OpenOptions::new().write(true).open(dest)?.sync_all()?;
    let Some(dir) = dest.parent().filter(|d| !d.as_os_str().is_empty()) else { return Ok(()) };
    folder_flushed(OpenOptions::new().write(true).custom_flags(FILE_FLAG_BACKUP_SEMANTICS).open(dir).and_then(|d| d.sync_all()))
}

/// The folder flush's result, once the clip itself is flushed.
fn folder_flushed(flushed: std::io::Result<()>) -> std::io::Result<()> {
    match flushed {
        Err(e) if matches!(e.raw_os_error(), Some(ERROR_INVALID_FUNCTION | ERROR_NOT_SUPPORTED | ERROR_ACCESS_DENIED)) => Ok(()),
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_moved_clip_and_its_folder_flush_on_this_machine() {
        let dir = crate::encoder::test_scratch::scratch("recall-flush", "dir");
        let clip = dir.join("G.mp4");
        std::fs::write(&clip, b"clip").unwrap();
        assert!(flush_dest(&clip).is_ok(), "{:?}", flush_dest(&clip));
        assert!(flush_dest(&dir.join("missing.mp4")).is_err(), "a missing clip is not flushed");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_folder_that_refuses_its_flush_counts_once_the_clip_is_flushed() {
        let os = std::io::Error::from_raw_os_error;
        assert!(folder_flushed(Err(os(5))).is_ok(), "ACCESS_DENIED (a NAS share): the file flush stands");
        assert!(folder_flushed(Err(os(1))).is_ok() && folder_flushed(Err(os(50))).is_ok(), "no folder flush (FAT)");
        assert!(folder_flushed(Err(os(32))).is_err(), "a sharing violation is retried");
        assert!(folder_flushed(Err(os(21))).is_err(), "a drive that is not ready is retried");
    }
}
