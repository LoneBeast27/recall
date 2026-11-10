//! Whole-file comparison of a clip and its kept copy, before the copy is deleted or made an
//! evictable cache copy (shared with the app's `services/storage/kept.rs`). Comparing only the
//! first 64 KiB and last 1 MiB let a clip with other bytes in the middle (a lost write) pass as
//! whole. Both files are opened shared (read, write and delete), so a reader never blocks the
//! recorder or a player; a file held without read sharing fails the open, and an error means
//! "not proven": the caller keeps the copy.
use std::io::Read;
use std::os::windows::fs::OpenOptionsExt;
use std::path::Path;

/// FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE.
const SHARE_ALL: u32 = 7;
/// Bounded buffer per file.
const CHUNK: usize = 1 << 20;

fn open(path: &Path) -> std::io::Result<std::fs::File> {
    std::fs::OpenOptions::new().read(true).share_mode(SHARE_ALL).open(path)
}

/// Fills `buf` as far as the file goes; the count read.
fn fill(file: &mut std::fs::File, buf: &mut [u8]) -> std::io::Result<usize> {
    let mut filled = 0;
    while filled < buf.len() {
        match file.read(&mut buf[filled..])? {
            0 => break,
            n => filled += n,
        }
    }
    Ok(filled)
}

/// `Ok(true)` when `a` and `b` hold the same bytes, both exactly `len` long. Any read error is
/// returned (the caller keeps the copy).
pub fn same_bytes(a: &Path, b: &Path, len: u64) -> std::io::Result<bool> {
    let (mut a, mut b) = (open(a)?, open(b)?);
    if a.metadata()?.len() != len || b.metadata()?.len() != len {
        return Ok(false);
    }
    let (mut x, mut y) = (vec![0u8; CHUNK], vec![0u8; CHUNK]);
    let mut seen = 0u64;
    loop {
        let (n, m) = (fill(&mut a, &mut x)?, fill(&mut b, &mut y)?);
        if n != m || x[..n] != y[..m] {
            return Ok(false);
        }
        seen += n as u64;
        if n < CHUNK {
            return Ok(seen == len);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_whole_file_is_compared_and_an_exclusive_holder_is_not_proof() {
        let dir = std::env::temp_dir().join(format!("recall-same-bytes-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let (a, b) = (dir.join("a"), dir.join("b"));
        let mut bytes = vec![5u8; (CHUNK * 2) + 17];
        std::fs::write(&a, &bytes).unwrap();
        std::fs::write(&b, &bytes).unwrap();
        let len = bytes.len() as u64;
        assert!(same_bytes(&a, &b, len).unwrap());
        // A reader holding it shared (as Recall's players do) does not get in the way.
        let shared = open(&a).unwrap();
        assert!(same_bytes(&a, &b, len).unwrap());
        drop(shared);
        bytes[CHUNK + 3] = 0;
        std::fs::write(&b, &bytes).unwrap();
        assert!(!same_bytes(&a, &b, len).unwrap(), "a middle byte differs");
        assert!(!same_bytes(&a, &b, len + 1).unwrap(), "another length");
        let held = std::fs::OpenOptions::new().read(true).share_mode(0).open(&a).unwrap();
        assert!(same_bytes(&a, &b, len).is_err(), "held without sharing: not proven");
        drop(held);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
