//! Is a clip already where the mover would put it? The mover deletes the staged copy when
//! it is, so "already in place" must mean the same clip, not just the same length.
//!
//! The check: same file (`file_id.rs`, asked by the mover first: the clip IS its destination
//! and is never deleted, and an unknown answer deletes nothing); else same size, same modification time (the mover stamps every copy with
//! the staged file's time, and a rename keeps it) and the same first and last MiB. A full
//! hash would read a whole multi-GB clip off the hard drive at every start for a recovered
//! move; two different recordings agreeing on size, the 100 ns save time and 2 MiB of
//! encoded bytes at both ends do not happen. Any doubt lands the clip beside the other file
//! (`-recovered`): a duplicate, never a loss.
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

const MIB: u64 = 1 << 20;

/// `dest` holds the same clip as `staged` (see the module doc).
pub(crate) fn same_clip(staged: &Path, dest: &Path) -> bool {
    compare(staged, dest, true)
}

/// [`same_clip`] without the modification time: for a copy the mover itself landed but could
/// not stamp (`mover_retry.rs`).
pub(crate) fn same_bytes(staged: &Path, dest: &Path) -> bool {
    compare(staged, dest, false)
}

fn compare(staged: &Path, dest: &Path, timed: bool) -> bool {
    let (Ok(a), Ok(b)) = (std::fs::metadata(staged), std::fs::metadata(dest)) else { return false };
    if a.len() != b.len() || (timed && (a.modified().ok().is_none() || a.modified().ok() != b.modified().ok())) {
        return false;
    }
    let len = a.len();
    let tail = len.saturating_sub(MIB);
    matches!((ends(staged, tail), ends(dest, tail)), (Some(x), Some(y)) if x == y)
}

/// The first MiB and the MiB from `tail` on.
fn ends(path: &Path, tail: u64) -> Option<(Vec<u8>, Vec<u8>)> {
    let mut file = File::open(path).ok()?;
    let mut head = Vec::new();
    (&mut file).take(MIB).read_to_end(&mut head).ok()?;
    file.seek(SeekFrom::Start(tail)).ok()?;
    let mut last = Vec::new();
    file.take(MIB).read_to_end(&mut last).ok()?;
    Some((head, last))
}

pub(crate) type IdentityFn = fn(&Path, &Path) -> Option<bool>;
pub(crate) type StatFn = fn(&Path) -> std::io::Result<std::fs::Metadata>;

/// `mover::Config::stat` in production.
pub(crate) fn stat_dest(path: &Path) -> std::io::Result<std::fs::Metadata> {
    std::fs::metadata(path)
}

/// `mover::Config::identity` in production: volume serial + file index (`file_id.rs`).
pub(crate) fn same_identity(a: &Path, b: &Path) -> Option<bool> {
    crate::file_id::same_file(a, b)
}
