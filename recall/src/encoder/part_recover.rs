//! Never lose a clip: what happens to a replay save's `<clip>.part` that did not become a clip.
//!
//! - At save time ([`after_failed_mux`]): a failed save's `.part` goes to the Recycle Bin only
//!   when the mux wrote no packet into it (a diagnostic line first). Any other `.part` stays on
//!   disk, listed for recovery (`in_progress`) with its packet index (`replay_index`).
//! - At engine start, a listed save is recovered from that list (`replay_rebuild`).
//! - [`recover_replay_parts`]: the one-time scan for `.part` files from builds before that list
//!   (`longrec::recover`), over the staging folder, the clip folder and its game folders. It
//!   never recycles: a `.part` whose MP4 is finished (its `moov` is written: the trailer went
//!   out, the rename did not) is renamed into a clip, `<name>.mp4` (or `<name>-recovered.mp4`);
//!   any other is kept as it is, logged once with a diagnostic line (an old save has no index).
//!
//! Long recordings' `__rec.mp4.part` files are fragmented and are `longrec::recover`'s, never
//! touched here.

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use crate::naming::{self, PART_EXT};

/// A failed save's `.part` (`saver::write_clip_to`): with no packet in it, nothing of the clip
/// is there: a diagnostic line and the Recycle Bin. Any other is kept (true) for the next start.
pub(crate) fn after_failed_mux(part: &Path, packets: u64, reason: &str, log: &dyn Fn(&str)) -> bool {
    if packets > 0 {
        log(&format!("saver: {} holds {packets} packets after \"{reason}\"; kept for recovery at the next start", part.display()));
        return true;
    }
    if !part.exists() {
        return false;
    }
    log(&format!("saver: {}: no packet was written (\"{reason}\")", part.display()));
    for line in super::recycle::dispose_unrecoverable(part, "no packet was written") {
        log(&format!("saver: {line}"));
    }
    false
}

/// What the start-up scan did with one replay `.part`.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Found {
    /// Finished MP4: renamed to this clip.
    Adopted { part: PathBuf, clip: PathBuf },
    /// Packets but no `moov`: kept as it is.
    Kept(PathBuf),
    /// No packet in it: kept (this scan never recycles), logged.
    Empty(PathBuf),
    /// Could not be renamed / read / deleted (left as it is).
    Error(PathBuf, String),
}

impl Found {
    /// The log line for this file.
    pub(crate) fn line(&self) -> String {
        match self {
            Found::Adopted { part, clip } => format!("recovered replay save {} as {}", part.display(), clip.display()),
            Found::Kept(part) => format!("replay save {} was cut off mid-write by an older build (no index); kept as it is, not deleted: boxes {}", part.display(), super::recycle::layout_summary(part)),
            Found::Empty(part) => format!("replay save {} from an older build holds no packet; kept as it is: boxes {}", part.display(), super::recycle::layout_summary(part)),
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

/// Every replay `.part` directly in `dir` or in one of its game folders (only folders Recall
/// made, `game_folder_rule`), oldest name first, except those the in-progress list names (they
/// are recovered from it). One modified within [`FRESH`] is left alone.
pub(crate) fn recover_replay_parts(dir: &Path) -> Vec<Found> {
    recover_replay_parts_older(dir, FRESH)
}

/// [`recover_replay_parts`] with the freshness window (tests: zero).
pub(crate) fn recover_replay_parts_older(dir: &Path, fresh: Duration) -> Vec<Found> {
    let old = |e: &std::fs::DirEntry| {
        let age = e.metadata().and_then(|m| m.modified()).ok().and_then(|t| SystemTime::now().duration_since(t).ok());
        age.is_some_and(|age| age >= fresh)
    };
    let listed: Vec<PathBuf> = super::in_progress::list().iter().map(super::in_progress::Entry::part).collect();
    let is_listed = |p: &Path| listed.iter().any(|l| l.to_string_lossy().eq_ignore_ascii_case(&p.to_string_lossy()));
    let folders = std::iter::once(dir.to_path_buf()).chain(crate::game_folder_rule::game_folders_marked(dir).into_iter().map(|(folder, _)| folder));
    let mut parts: Vec<PathBuf> = folders
        .flat_map(|folder| std::fs::read_dir(folder).into_iter().flatten().flatten())
        .filter(|e| e.file_type().is_ok_and(|t| t.is_file()) && is_replay_part(&e.file_name().to_string_lossy()) && old(e))
        .map(|e| e.path())
        .filter(|p| !is_listed(p))
        .collect();
    parts.sort();
    parts.into_iter().map(|part| recover_one(&part)).collect()
}

fn recover_one(part: &Path) -> Found {
    match layout(part) {
        Err(e) => Found::Error(part.to_path_buf(), format!("not read: {e}")),
        // A whole `moov` that still does not play is kept, never renamed into the library.
        Ok(Layout::Finished) if !plays(part) => Found::Kept(part.to_path_buf()),
        Ok(Layout::Finished) => {
            let clip = naming::recovered_path(part, Path::exists);
            match std::fs::rename(part, &clip) {
                Ok(()) => Found::Adopted { part: part.to_path_buf(), clip },
                Err(e) => Found::Error(part.to_path_buf(), format!("not renamed: {e}")),
            }
        }
        Ok(Layout::Unindexed) => Found::Kept(part.to_path_buf()),
        Ok(Layout::NoPackets) => Found::Empty(part.to_path_buf()),
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

/// The file opens and at least one video packet demuxes (a finished file is only adopted then).
pub(crate) fn plays(path: &Path) -> bool {
    let _ = ffmpeg_next::init();
    let Ok(mut input) = ffmpeg_next::format::input(&path) else { return false };
    let Some(video) = input.streams().best(ffmpeg_next::media::Type::Video).map(|s| s.index()) else { return false };
    input.packets().any(|(s, _)| s.index() == video)
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
            // Finished only with the whole `moov` in the file: a kill during the trailer tears it.
            b"moov" if size >= header && pos + size <= len => return Ok(Layout::Finished),
            b"moov" => return Ok(Layout::Unindexed),
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
