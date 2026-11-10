//! Never lose a clip: what happens to a replay save's `<clip>.part` that did not become a clip.
//!
//! - At save time ([`after_failed_mux`]): a failed save's `.part` is deleted only when the mux
//!   wrote no packet into it (logged). Any other `.part` stays on disk.
//! - At engine start ([`recover_replay_parts`], over the staging folder and the clip folder): a
//!   `.part` whose MP4 is finished (its `moov` is written: the trailer went out, the rename did
//!   not) is renamed into a clip, `<name>.mp4` (or `<name>-recovered.mp4` when that is taken).
//!   One that holds packets but no `moov` (the engine was killed mid-write) cannot be played
//!   by a rename: it is kept as it is, and logged at every start. One with no packet at all
//!   (killed right after the header) is deleted, logged.
//!
//! Long recordings' `__rec.mp4.part` files are fragmented and are `longrec::recover_parts`'s,
//! never touched here.

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use crate::naming::{self, PART_EXT};

/// A failed save's `.part` (`saver::write_clip_to`): deleted when `packets` is 0, else kept.
pub(crate) fn after_failed_mux(part: &Path, packets: u64, reason: &str, log: &dyn Fn(&str)) {
    if packets > 0 {
        log(&format!("saver: {} holds {packets} packets after \"{reason}\"; kept for recovery at the next start", part.display()));
        return;
    }
    if !part.exists() {
        return;
    }
    match std::fs::remove_file(part) {
        Ok(()) => log(&format!("saver: removed {}: no packet was written (\"{reason}\")", part.display())),
        Err(e) => log(&format!("saver: {} (no packet written) not removed: {e}", part.display())),
    }
}

/// What the start-up scan did with one replay `.part`.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Found {
    /// Finished MP4: renamed to this clip.
    Adopted { part: PathBuf, clip: PathBuf },
    /// Packets but no `moov`: kept as it is.
    Kept(PathBuf),
    /// No packet in it: deleted.
    Empty(PathBuf),
    /// Could not be renamed / read / deleted (left as it is).
    Error(PathBuf, String),
}

impl Found {
    /// The log line for this file.
    pub(crate) fn line(&self) -> String {
        match self {
            Found::Adopted { part, clip } => format!("recovered replay save {} as {}", part.display(), clip.display()),
            Found::Kept(part) => format!("replay save {} was cut off mid-write (no index); kept as it is, not deleted", part.display()),
            Found::Empty(part) => format!("removed {}: an unfinished replay save with no packet in it", part.display()),
            Found::Error(part, e) => format!("replay save {} left as it is: {e}", part.display()),
        }
    }
}

/// A replay save's in-progress name: `*.mp4.part`, not a long recording's.
pub(crate) fn is_replay_part(file_name: &str) -> bool {
    let lower = file_name.to_ascii_lowercase();
    lower.strip_suffix(PART_EXT).is_some_and(|stem| stem.ends_with(".mp4")) && !naming::is_longrec_part(&lower)
}

/// A `.part` written this recently may be this engine's own save in progress: left for the
/// next start (the scan runs beside the saver).
pub(crate) const FRESH: Duration = Duration::from_secs(60);

/// Every replay `.part` directly in `dir` (see the module doc), oldest name first. One
/// modified within [`FRESH`] is left alone.
pub(crate) fn recover_replay_parts(dir: &Path) -> Vec<Found> {
    recover_replay_parts_older(dir, FRESH)
}

/// [`recover_replay_parts`] with the freshness window (tests: zero).
pub(crate) fn recover_replay_parts_older(dir: &Path, fresh: Duration) -> Vec<Found> {
    let old = |e: &std::fs::DirEntry| {
        let age = e.metadata().and_then(|m| m.modified()).ok().and_then(|t| SystemTime::now().duration_since(t).ok());
        age.is_some_and(|age| age >= fresh)
    };
    let Ok(listing) = std::fs::read_dir(dir) else { return Vec::new() };
    let mut parts: Vec<PathBuf> = listing
        .flatten()
        .filter(|e| e.file_type().is_ok_and(|t| t.is_file()) && is_replay_part(&e.file_name().to_string_lossy()) && old(e))
        .map(|e| e.path())
        .collect();
    parts.sort();
    parts.into_iter().map(|part| recover_one(&part)).collect()
}

fn recover_one(part: &Path) -> Found {
    match layout(part) {
        Err(e) => Found::Error(part.to_path_buf(), format!("not read: {e}")),
        Ok(Layout::Finished) => {
            let clip = naming::recovered_path(part, Path::exists);
            match std::fs::rename(part, &clip) {
                Ok(()) => Found::Adopted { part: part.to_path_buf(), clip },
                Err(e) => Found::Error(part.to_path_buf(), format!("not renamed: {e}")),
            }
        }
        Ok(Layout::Unindexed) => Found::Kept(part.to_path_buf()),
        Ok(Layout::NoPackets) => match std::fs::remove_file(part) {
            Ok(()) => Found::Empty(part.to_path_buf()),
            Err(e) => Found::Error(part.to_path_buf(), format!("not removed: {e}")),
        },
    }
}

/// What an MP4 `.part` holds, from its top-level boxes.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Layout {
    /// A `moov` is written: a whole, playable file.
    Finished,
    /// No `moov`, and media data (or anything unknown): kept.
    Unindexed,
    /// Only the header boxes and an empty `mdat` (or none): nothing of the clip in it.
    NoPackets,
}

/// Read the top-level box headers of `path` (seeking past every payload).
pub(crate) fn layout(path: &Path) -> std::io::Result<Layout> {
    let mut file = File::open(path)?;
    let len = file.metadata()?.len();
    let mut pos = 0u64;
    let mut media = false;
    while pos + 8 <= len {
        file.seek(SeekFrom::Start(pos))?;
        let mut head = [0u8; 16];
        file.read_exact(&mut head[..8])?;
        let size32 = u32::from_be_bytes([head[0], head[1], head[2], head[3]]) as u64;
        let kind = [head[4], head[5], head[6], head[7]];
        let (header, size) = match size32 {
            0 => (8, len - pos),
            1 if pos + 16 <= len => {
                file.read_exact(&mut head[8..16])?;
                (16, u64::from_be_bytes(head[8..16].try_into().unwrap_or_default()))
            }
            1 => return Ok(Layout::Unindexed),
            n => (8, n),
        };
        match &kind {
            b"moov" => return Ok(Layout::Finished),
            b"mdat" => media |= pos + header < len && size > header,
            b"ftyp" | b"free" | b"wide" | b"skip" => {}
            _ => media = true,
        }
        if size < header {
            return Ok(Layout::Unindexed);
        }
        pos = pos.saturating_add(size);
    }
    // Trailing bytes too short for a box header still came from the mux.
    Ok(if media || pos < len { Layout::Unindexed } else { Layout::NoPackets })
}

#[cfg(test)]
#[path = "part_recover_tests.rs"]
pub(crate) mod tests;
#[cfg(test)]
#[path = "part_recover_restore_tests.rs"]
mod restore_tests;
