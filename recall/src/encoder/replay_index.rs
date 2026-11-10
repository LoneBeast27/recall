//! A replay save's packet index, so a save cut off mid-write (a kill, a crash, a power cut) is
//! rebuilt into a playable clip at the next start (`replay_rebuild`), never parked.
//!
//! Why it works: the MP4 muxer writes a plain (non-fragmented) clip's samples into `mdat` as
//! they are given (`mux_mp4` uses `write_frame`: no interleaver holding packets back) and keeps
//! only the sample tables (`moov`) for the trailer. Every packet of a save is known before the
//! mux starts, and the one change the muxer makes to a packet (H.264/HEVC start codes become
//! 4-byte lengths) follows from its bytes, so the whole layout is planned up front
//! (`mux_index::plan`) and on disk before the first packet; the mux checks each packet against
//! its plan and writes records through from the first difference (`replay_index_io`).
//!
//! - `<base>.hdr.mp4`: the clip's streams as a header-only fragmented MP4 (`ftyp` + an empty
//!   `moov` with every sample entry: codec, parameter sets, colour, track titles, tags), written
//!   by the same stream setup as the clip itself.
//! - `<base>.idx`: [`MAGIC`], the [`Head`] (JSON, length-prefixed), then one 32-byte [`Record`]
//!   per packet in write order: the plan, then a marker and the records written through, if any.
//! - The base is a fixed slot under `%LOCALAPPDATA%\Recall\replay-index\` (the saver writes one
//!   save at a time), overwritten by the next save: nothing to delete after a good save. A save
//!   left for recovery gets its own copy first ([`claim`]: a rename).
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

pub(crate) const MAGIC: &[u8; 8] = b"RCLIDX03";

#[path = "replay_index_io.rs"]
mod io;
pub(crate) use io::{begin, read, Writer};
#[cfg(test)]
pub(crate) use io::MARKER;

/// One packet as the muxer wrote it: where its sample sits in the `.part` (`offset`, `size`),
/// and its pts and duration in its stream's input time base ([`Head::time_bases`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Record {
    pub stream: u8,
    pub key: bool,
    pub size: u32,
    pub offset: u64,
    pub pts: i64,
    pub dur: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Head {
    /// The `.part` this index belongs to.
    pub part: String,
    /// Each stream's input time base (num, den), in stream order.
    pub time_bases: Vec<(i32, i32)>,
    /// Records planned up front (0: none, written through from the start).
    #[serde(default)]
    pub planned: usize,
}

/// What writing one index cost.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct Cost {
    pub bytes: u64,
    pub ms: f64,
    /// Of `ms`: writing the header, the head and the plan before the first packet.
    pub plan_ms: f64,
}

#[cfg(not(test))]
static RUN_SLOT: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
#[cfg(test)]
thread_local! { static RUN_SLOT: std::cell::Cell<bool> = const { std::cell::Cell::new(false) }; }

/// The list could not be read at start, so an unclaimed save may still need the slot: this
/// run's saves use a slot of their own ([`slot`]).
pub(crate) fn use_run_slot() {
    #[cfg(not(test))]
    RUN_SLOT.store(true, std::sync::atomic::Ordering::Relaxed);
    #[cfg(test)]
    RUN_SLOT.with(|r| r.set(true));
}

fn run_slot() -> bool {
    #[cfg(not(test))]
    return RUN_SLOT.load(std::sync::atomic::Ordering::Relaxed);
    #[cfg(test)]
    return RUN_SLOT.with(std::cell::Cell::get);
}

/// The production slot: `%LOCALAPPDATA%\Recall\replay-index\save` (tests: one per thread), or
/// `save-p<pid>` for a run that could not read the list at start ([`use_run_slot`]).
pub(crate) fn slot() -> Option<PathBuf> {
    let dir = crate::user_dirs::localappdata()?.join("Recall").join("replay-index");
    #[cfg(test)]
    let mut name = format!("save-{:?}", std::thread::current().id()).replace(['(', ')'], "");
    #[cfg(not(test))]
    let mut name = "save".to_string();
    if run_slot() {
        name.push_str(&format!("-p{}", std::process::id()));
    }
    Some(crate::user_dirs::checked(dir.join(name)))
}

pub(crate) fn idx_of(base: &Path) -> PathBuf {
    with_suffix(base, ".idx")
}

pub(crate) fn hdr_of(base: &Path) -> PathBuf {
    with_suffix(base, ".hdr.mp4")
}

fn with_suffix(base: &Path, suffix: &str) -> PathBuf {
    let mut s = base.as_os_str().to_owned();
    s.push(suffix);
    PathBuf::from(s)
}

/// A save left for recovery keeps its index: the slot's files are renamed to a base of their
/// own (`<slot>-<unix ms>`), so the next save cannot overwrite them. The new base.
pub(crate) fn claim(base: &Path) -> Result<PathBuf, String> {
    let stamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis()).unwrap_or(0);
    let own = with_suffix(base, &format!("-{stamp}"));
    for (from, to) in [(idx_of(base), idx_of(&own)), (hdr_of(base), hdr_of(&own))] {
        if from.exists() {
            std::fs::rename(&from, &to).map_err(|e| format!("keep {}: {e}", from.display()))?;
        }
    }
    Ok(own)
}

/// At a clean quit: a run slot (`save-p<pid>`, [`use_run_slot`]) is this run's own temp files
/// (every save of the run finished or claimed its copy): to the Recycle Bin, as other temp files.
pub(crate) fn drop_run_slot() {
    if !run_slot() {
        return;
    }
    let Some(base) = slot() else { return };
    for file in [idx_of(&base), hdr_of(&base)] {
        if file.exists() {
            let _ = super::recycle::recycle(&file);
        }
    }
}

/// Tests: this thread's saves back on the shared slot.
#[cfg(test)]
pub(crate) fn tests_reset_run_slot() {
    RUN_SLOT.with(|r| r.set(false));
}

/// Whether `base` is the production slot (not a claimed copy).
pub(crate) fn is_slot(base: &Path) -> bool {
    slot().is_some_and(|s| super::in_progress::same_path(&s.to_string_lossy(), &base.to_string_lossy()))
}

/// Whether `base` is one of Recall's own index bases: the slot or a claimed copy of it, in the
/// replay-index folder. Nothing else is ever read as an index or recycled as one.
pub(crate) fn is_index_base(base: &Path) -> bool {
    let (Some(slot), Some(dir)) = (slot(), base.parent()) else { return false };
    let slot_name = slot.file_name().map(|n| n.to_string_lossy().to_lowercase()).unwrap_or_default();
    let name = base.file_name().map(|n| n.to_string_lossy().to_lowercase()).unwrap_or_default();
    slot.parent().is_some_and(|s| super::in_progress::same_path(&s.to_string_lossy(), &dir.to_string_lossy())) && name.starts_with(&slot_name)
}

/// Whether the index at `base` belongs to the save into `part`.
pub(crate) fn belongs_to(base: &Path, part: &Path) -> bool {
    read(base).is_ok_and(|(head, _)| super::in_progress::same_path(&head.part, &part.to_string_lossy()))
}

#[cfg(test)]
#[path = "replay_restore_tests.rs"]
pub(crate) mod tests;
