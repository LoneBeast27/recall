//! Copying a prepared export into `Recall\Edited`: in chunks, so Cancel stops a multi-GB copy
//! to a hard drive, and with the failures told apart, so a full or locked folder keeps the
//! prepared file (an encode from scratch would fail the same way minutes later).
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

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

/// Copies `from` to the hidden `temp`, then moves it to `to`, or to the next free export number
/// when a file appeared at `to` meanwhile: an existing file is never replaced. Returns where it
/// landed. `temp` never stays behind.
pub fn publish_copy(from: &Path, temp: &Path, to: &Path, stop: fn() -> bool) -> Result<PathBuf, Delivery> {
    let result = copy_chunks(from, temp, stop).and_then(|()| publish(temp, to));
    if result.is_err() {
        let _ = std::fs::remove_file(temp);
    }
    result
}

/// Moves `temp` to `to` without replacing anything; on a taken name, the next free number.
fn publish(temp: &Path, to: &Path) -> Result<PathBuf, Delivery> {
    let mut target = to.to_path_buf();
    for _ in 0..100 {
        match move_no_replace(temp, &target) {
            Ok(()) => return Ok(target),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists || error.raw_os_error() == Some(183) => {
                target = crate::backend::name_after(&target).ok_or_else(|| Delivery::Failed(format!("publish prepared export: {error}")))?;
            }
            Err(error) => return Err(classify(&error, "publish prepared export")),
        }
    }
    Err(Delivery::Failed("publish prepared export: no free export name".into()))
}

/// `MoveFileExW` without `MOVEFILE_REPLACE_EXISTING` (`std::fs::rename` replaces on Windows).
fn move_no_replace(from: &Path, to: &Path) -> std::io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use windows::Win32::Storage::FileSystem::{MOVEFILE_WRITE_THROUGH, MoveFileExW};
    let wide = |path: &Path| path.as_os_str().encode_wide().chain(Some(0)).collect::<Vec<u16>>();
    let (from, to) = (wide(from), wide(to));
    // SAFETY: both are NUL-terminated wide strings that outlive the call.
    unsafe { MoveFileExW(windows::core::PCWSTR(from.as_ptr()), windows::core::PCWSTR(to.as_ptr()), MOVEFILE_WRITE_THROUGH) }
        .map_err(|error| std::io::Error::from_raw_os_error(error.code().0 & 0xFFFF))
}

fn copy_chunks(from: &Path, to: &Path, stop: fn() -> bool) -> Result<(), Delivery> {
    let mut source = std::fs::File::open(from).map_err(|e| Delivery::Failed(format!("open prepared export: {e}")))?;
    let create = || std::fs::OpenOptions::new().write(true).create_new(true).open(to);
    // A hidden copy left at this name by a crash is ours (the start sweep missed it): replace it.
    let created = match create() {
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            let _ = std::fs::remove_file(to);
            create()
        }
        other => other,
    };
    let mut target = created.map_err(|e| classify(&e, "copy prepared export"))?;
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
        assert_eq!(publish_copy(&from, &temp, &to, || false), Ok(to.clone()));
        assert_eq!(std::fs::metadata(&to).unwrap().len(), 3 << 20);
        assert!(!temp.exists());

        // A folder in the way of the published name: never replaced; the copy takes the next number.
        let blocked = sandbox.join("blocked-1.mp4");
        std::fs::create_dir_all(&blocked).unwrap();
        let result = publish_copy(&from, &temp, &blocked, || false);
        assert_eq!(result, Ok(sandbox.join("blocked-2.mp4")));
        assert!(from.exists() && blocked.is_dir() && !temp.exists(), "the prepared file stays, the hidden copy goes");

        // A folder that is not there: another failure (Export encodes instead); nothing stays.
        let result = publish_copy(&from, &temp, &sandbox.join("missing").join("out.mp4"), || false);
        assert!(matches!(result, Err(Delivery::Failed(_))), "{result:?}");
        assert!(from.exists() && !temp.exists());

        STOP.store(true, Ordering::SeqCst);
        let result = publish_copy(&from, &temp, &sandbox.join("cancelled.mp4"), || STOP.load(Ordering::SeqCst));
        assert_eq!(result, Err(Delivery::Cancelled));
        assert!(!temp.exists() && !sandbox.join("cancelled.mp4").exists() && from.exists());
    }

    /// R2-4: a file that appeared at the chosen name during the copy (a restored older export,
    /// another instance) is never replaced; the copy takes the next number.
    #[test]
    fn the_copy_never_replaces_a_file_at_the_chosen_name() {
        let sandbox = crate::test_temp::TempDir::sandbox("recall-pre-encode-no-replace");
        let from = sandbox.join("prepared.mp4");
        std::fs::write(&from, b"new export").unwrap();
        let to = sandbox.join("clip-edited-60fps-1.mp4");
        std::fs::write(&to, b"older export").unwrap();
        let temp = sandbox.join(".clip-edited-60fps-1.mp4.recall-pre-encode");
        let published = publish_copy(&from, &temp, &to, || false);
        assert_eq!(std::fs::read(&to).unwrap(), b"older export", "the file at the name is untouched");
        let published = published.expect("the copy lands under another name");
        assert_eq!(published, sandbox.join("clip-edited-60fps-2.mp4"));
        assert_eq!(std::fs::read(&published).unwrap(), b"new export");
        assert!(!temp.exists());

        // A hidden copy left by a crash at the same temp name is ours: it is replaced.
        std::fs::write(&temp, b"stale").unwrap();
        let again = publish_copy(&from, &temp, &sandbox.join("clip-edited-60fps-3.mp4"), || false);
        assert_eq!(again, Ok(sandbox.join("clip-edited-60fps-3.mp4")));
        assert!(!temp.exists());
    }

    #[test]
    fn full_denied_and_locked_keep_the_prepared_file() {
        let kept = |code| matches!(classify(&std::io::Error::from_raw_os_error(code), "x"), Delivery::Kept(_));
        assert!(kept(112) && kept(39) && kept(5) && kept(32) && kept(33));
        assert!(!kept(2), "a missing folder is another failure");
    }
}
