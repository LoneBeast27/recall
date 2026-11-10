//! A replay save's packet index, so a save cut off mid-write (a kill, a crash, a power cut) is
//! rebuilt into a playable clip at the next start (`replay_rebuild`), never parked.
//!
//! Why it works: the MP4 muxer writes a plain (non-fragmented) clip's samples into `mdat` as
//! they are given (`mux_mp4` uses `write_frame`: no interleaver holding packets back) and keeps
//! only the sample tables (`moov`) for the trailer. The mux records where each sample landed
//! (its offset and size as written: the muxer may rewrite a packet, e.g. H.264 start codes to
//! lengths), so a cut-off `.part` still says where every whole packet is.
//!
//! - `<base>.hdr.mp4`: the clip's streams as a header-only fragmented MP4 (`ftyp` + an empty
//!   `moov` with every sample entry: codec, parameter sets, colour, track titles, tags), written
//!   by the same stream setup as the clip itself.
//! - `<base>.idx`: [`MAGIC`], the [`Head`] (JSON, length-prefixed), then one 32-byte [`Record`]
//!   per packet in write order. The head is on disk before the first packet goes into the
//!   `.part`; records go through a 1 MiB buffer handed to Windows at each video keyframe (or every
//!   [`FLUSH_EVERY`] records), and the file is flushed to disk once, at the end (no per-packet
//!   flush). A kill keeps what Windows had; a torn last record is ignored.
//! - The base is a fixed slot under `%LOCALAPPDATA%\Recall\replay-index\` (the saver writes one
//!   save at a time), overwritten by the next save: nothing to delete after a good save. A save
//!   left for recovery gets its own copy first ([`claim`]: a rename).
use std::io::{BufWriter, Read, Write};
use std::path::{Path, PathBuf};
use std::time::Instant;

use serde::{Deserialize, Serialize};

pub(crate) const MAGIC: &[u8; 8] = b"RCLIDX02";
const RECORD: usize = 32;
/// Records between two hand-overs to Windows when no keyframe comes.
pub(crate) const FLUSH_EVERY: usize = 512;

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
}

/// What writing one index cost.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct Cost {
    pub bytes: u64,
    pub ms: f64,
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

/// An index being written (see the module doc).
pub(crate) struct Writer {
    out: BufWriter<std::fs::File>,
    idx: PathBuf,
    hdr: PathBuf,
    since_flush: usize,
    error: Option<String>,
    /// Time spent in the index alone (the header, the head, the records, the flushes).
    busy: std::time::Duration,
}

/// Start the index for `part` at `base`: `header` writes the header-only MP4 at the path it is
/// given, then the head goes to disk, before any packet.
pub(crate) fn begin(base: &Path, part: &Path, header: impl FnOnce(&Path) -> Result<(), String>, time_bases: &[(i32, i32)]) -> Result<Writer, String> {
    let started = Instant::now();
    if let Some(dir) = base.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("create {}: {e}", dir.display()))?;
    }
    let (idx, hdr) = (idx_of(base), hdr_of(base));
    header(&hdr)?;
    let part = std::path::absolute(part).unwrap_or_else(|_| part.to_path_buf());
    let head = serde_json::to_vec(&Head { part: part.to_string_lossy().into_owned(), time_bases: time_bases.to_vec() }).map_err(|e| e.to_string())?;
    let file = std::fs::File::create(&idx).map_err(|e| format!("create {}: {e}", idx.display()))?;
    let mut out = BufWriter::with_capacity(1 << 20, file);
    out.write_all(MAGIC)
        .and_then(|()| out.write_all(&(head.len() as u32).to_le_bytes()))
        .and_then(|()| out.write_all(&head))
        .and_then(|()| out.flush())
        .and_then(|()| out.get_ref().sync_data())
        .map_err(|e| format!("write {}: {e}", idx.display()))?;
    Ok(Writer { out, idx, hdr, since_flush: 0, error: None, busy: started.elapsed() })
}

impl Writer {
    /// One written packet. A write error stops the index there (kept: what is in it still holds).
    pub(crate) fn push(&mut self, r: Record) {
        if self.error.is_some() {
            return;
        }
        let at = Instant::now();
        let mut rec = [0u8; RECORD];
        rec[0] = r.stream;
        rec[1] = u8::from(r.key);
        rec[4..8].copy_from_slice(&r.size.to_le_bytes());
        rec[8..16].copy_from_slice(&r.offset.to_le_bytes());
        rec[16..24].copy_from_slice(&r.pts.to_le_bytes());
        rec[24..32].copy_from_slice(&r.dur.to_le_bytes());
        self.since_flush += 1;
        let hand_over = (r.key && r.stream == 0) || self.since_flush >= FLUSH_EVERY;
        let written = self.out.write_all(&rec).and_then(|()| if hand_over { self.out.flush() } else { Ok(()) });
        if hand_over {
            self.since_flush = 0;
        }
        if let Err(e) = written {
            self.error = Some(format!("write {}: {e}", self.idx.display()));
        }
        self.busy += at.elapsed();
    }

    /// Flush to disk once. What it cost (bytes of both files, time in the index), or the error.
    pub(crate) fn finish(mut self) -> Result<Cost, String> {
        let at = Instant::now();
        if let Some(e) = self.error.take() {
            return Err(e);
        }
        self.out.flush().and_then(|()| self.out.get_ref().sync_data()).map_err(|e| format!("write {}: {e}", self.idx.display()))?;
        self.busy += at.elapsed();
        let size = |p: &Path| std::fs::metadata(p).map(|m| m.len()).unwrap_or(0);
        Ok(Cost { bytes: size(&self.idx) + size(&self.hdr), ms: self.busy.as_secs_f64() * 1000.0 })
    }
}

/// The index at `base`: its head and records (in write order).
pub(crate) fn read(base: &Path) -> Result<(Head, Vec<Record>), String> {
    let idx = idx_of(base);
    let mut bytes = Vec::new();
    std::fs::File::open(&idx).and_then(|mut f| f.read_to_end(&mut bytes)).map_err(|e| format!("read {}: {e}", idx.display()))?;
    let bad = || format!("{} is not a packet index", idx.display());
    if bytes.get(..8) != Some(&MAGIC[..]) {
        return Err(bad());
    }
    let u32_at = |at: usize| bytes.get(at..at + 4).map(|b| u32::from_le_bytes(b.try_into().unwrap_or_default()) as usize);
    let head_len = u32_at(8).ok_or_else(bad)?;
    let head: Head = serde_json::from_slice(bytes.get(12..12 + head_len).ok_or_else(bad)?).map_err(|_| bad())?;
    // A torn last record (a kill mid-write) is left out.
    let records = bytes[12 + head_len..]
        .chunks_exact(RECORD)
        .map(|r| Record {
            stream: r[0],
            key: r[1] != 0,
            size: u32::from_le_bytes(r[4..8].try_into().unwrap_or_default()),
            offset: u64::from_le_bytes(r[8..16].try_into().unwrap_or_default()),
            pts: i64::from_le_bytes(r[16..24].try_into().unwrap_or_default()),
            dur: i64::from_le_bytes(r[24..32].try_into().unwrap_or_default()),
        })
        .collect();
    Ok((head, records))
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
