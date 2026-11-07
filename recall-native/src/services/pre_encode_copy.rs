//! Copying a prepared export into `Recall\Edited`: in chunks, so Cancel stops a multi-GB copy
//! to a hard drive, and with the failures told apart, so a full or locked folder keeps the
//! prepared file (an encode from scratch would fail the same way minutes later).
use std::io::{Read, Write};
use std::path::Path;

/// How a delivery of the prepared file failed.
#[derive(Debug, PartialEq)]
pub enum Delivery {
    /// Cancel was pressed during the copy.
    Cancelled,
    /// The export folder is full, read-only or locked: keep the prepared file.
    Kept(String),
    /// Anything else: Export encodes from scratch instead.
    Failed(String),
}

const CHUNK: usize = 8 << 20;

/// Copies `from` to the hidden `temp`, then renames it to `to`. `temp` never stays behind.
pub fn publish_copy(from: &Path, temp: &Path, to: &Path, stop: fn() -> bool) -> Result<(), Delivery> {
    let result = copy_chunks(from, temp, stop).and_then(|()| {
        std::fs::rename(temp, to).map_err(|e| classify(&e, "publish prepared export"))
    });
    if result.is_err() {
        let _ = std::fs::remove_file(temp);
    }
    result
}

fn copy_chunks(from: &Path, to: &Path, stop: fn() -> bool) -> Result<(), Delivery> {
    let mut source = std::fs::File::open(from).map_err(|e| Delivery::Failed(format!("open prepared export: {e}")))?;
    let mut target = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(to)
        .map_err(|e| classify(&e, "copy prepared export"))?;
    let mut buffer = vec![0u8; CHUNK];
    loop {
        if stop() {
            return Err(Delivery::Cancelled);
        }
        let read = source.read(&mut buffer).map_err(|e| Delivery::Failed(format!("read prepared export: {e}")))?;
        if read == 0 {
            break;
        }
        target.write_all(&buffer[..read]).map_err(|e| classify(&e, "copy prepared export"))?;
    }
    target.sync_all().map_err(|e| classify(&e, "copy prepared export"))
}

/// Full, read-only, denied or locked: the same would stop an encode into that folder.
fn classify(error: &std::io::Error, what: &str) -> Delivery {
    use std::io::ErrorKind::{PermissionDenied, ReadOnlyFilesystem, StorageFull};
    // ERROR_ACCESS_DENIED, ERROR_SHARING_VIOLATION, ERROR_LOCK_VIOLATION, ERROR_HANDLE_DISK_FULL, ERROR_DISK_FULL.
    let os = matches!(error.raw_os_error(), Some(5 | 32 | 33 | 39 | 112));
    let message = format!("{what}: {error}");
    if os || matches!(error.kind(), StorageFull | PermissionDenied | ReadOnlyFilesystem) { Delivery::Kept(message) } else { Delivery::Failed(message) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering};

    static STOP: AtomicBool = AtomicBool::new(false);

    #[test]
    fn a_copy_publishes_and_a_blocked_or_cancelled_one_leaves_the_prepared_file() {
        let sandbox = crate::test_temp::TempDir::sandbox("recall-pre-encode-copy");
        let from = sandbox.join("prepared.mp4");
        std::fs::write(&from, vec![7u8; 3 << 20]).unwrap();
        let (temp, to) = (sandbox.join(".out.mp4.recall-pre-encode"), sandbox.join("out.mp4"));
        assert_eq!(publish_copy(&from, &temp, &to, || false), Ok(()));
        assert_eq!(std::fs::metadata(&to).unwrap().len(), 3 << 20);
        assert!(!temp.exists());

        // A folder in the way of the published name: the rename is denied.
        let blocked = sandbox.join("blocked.mp4");
        std::fs::create_dir_all(&blocked).unwrap();
        let result = publish_copy(&from, &temp, &blocked, || false);
        assert!(matches!(result, Err(Delivery::Kept(_))), "{result:?}");
        assert!(from.exists() && blocked.is_dir() && !temp.exists(), "the prepared file stays, the hidden copy goes");

        STOP.store(true, Ordering::SeqCst);
        let result = publish_copy(&from, &temp, &sandbox.join("cancelled.mp4"), || STOP.load(Ordering::SeqCst));
        assert_eq!(result, Err(Delivery::Cancelled));
        assert!(!temp.exists() && !sandbox.join("cancelled.mp4").exists() && from.exists());
    }

    #[test]
    fn full_denied_and_locked_keep_the_prepared_file() {
        let kept = |code| matches!(classify(&std::io::Error::from_raw_os_error(code), "x"), Delivery::Kept(_));
        assert!(kept(112) && kept(39) && kept(5) && kept(32) && kept(33));
        assert!(!kept(2), "a missing folder is another failure");
    }
}
