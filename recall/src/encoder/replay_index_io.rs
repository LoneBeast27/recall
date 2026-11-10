//! The packet index file (part of `replay_index`): writing it during a save and reading it back.
//!
//! - The head and the **plan** go to Windows (and to disk, one flush) before the first packet:
//!   one record per packet of the save with its predicted place in the `.part` (`mux_index`
//!   predicts what the muxer writes). A process kill then loses nothing the `.part` holds.
//! - During the mux each packet's real place is checked against its planned record. On the
//!   first difference a marker ([`MARKER`], "the plan holds up to record k") is written and
//!   every later record is written through: only after its packet is handed to Windows
//!   (`mux_index::hand_over`), one 32-byte write each, so a record never claims bytes the `.part`
//!   does not have. A save with no plan (AV1: the muxer drops some OBUs, so sizes cannot be
//!   predicted) is written through from its first packet.
//! - No per-packet flush to disk: one at the start (head + plan) and one at the end.
//! - Reading: the plan up to its marker, then the records written through. A torn last record
//!   (a kill mid-write) is left out. Every record is still checked against the `.part` itself
//!   before it is used (`replay_rebuild`).
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use super::{hdr_of, idx_of, Cost, Head, Record, MAGIC};

const RECORD: usize = 32;
/// The stream byte of a marker record: the plan holds for the records before its `offset`.
pub(crate) const MARKER: u8 = 0xFE;

fn encode(r: &Record) -> [u8; RECORD] {
    let mut rec = [0u8; RECORD];
    rec[0] = r.stream;
    rec[1] = u8::from(r.key);
    rec[4..8].copy_from_slice(&r.size.to_le_bytes());
    rec[8..16].copy_from_slice(&r.offset.to_le_bytes());
    rec[16..24].copy_from_slice(&r.pts.to_le_bytes());
    rec[24..32].copy_from_slice(&r.dur.to_le_bytes());
    rec
}

fn decode(r: &[u8]) -> Record {
    Record {
        stream: r[0],
        key: r[1] != 0,
        size: u32::from_le_bytes(r[4..8].try_into().unwrap_or_default()),
        offset: u64::from_le_bytes(r[8..16].try_into().unwrap_or_default()),
        pts: i64::from_le_bytes(r[16..24].try_into().unwrap_or_default()),
        dur: i64::from_le_bytes(r[24..32].try_into().unwrap_or_default()),
    }
}

/// An index being written (see the module doc).
pub(crate) struct Writer {
    file: std::fs::File,
    idx: PathBuf,
    hdr: PathBuf,
    /// Each planned record's (offset, size); empty once the plan stopped holding.
    plan: Vec<(u64, u32)>,
    live: bool,
    error: Option<String>,
    /// Time in the index: the plan (header, head, records, the one flush), then the rest.
    plan_time: Duration,
    busy: Duration,
}

/// Start the index for `part` at `base`: `header` writes the header-only MP4 at the path it is
/// given; the head and `plan` (one record per packet, or none) then go to disk before any packet.
pub(crate) fn begin(base: &Path, part: &Path, header: impl FnOnce(&Path) -> Result<(), String>, time_bases: &[(i32, i32)], plan: Option<&[Record]>) -> Result<Writer, String> {
    let started = Instant::now();
    if let Some(dir) = base.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("create {}: {e}", dir.display()))?;
    }
    let (idx, hdr) = (idx_of(base), hdr_of(base));
    header(&hdr)?;
    let part = std::path::absolute(part).unwrap_or_else(|_| part.to_path_buf());
    let planned = plan.map_or(0, <[Record]>::len);
    let head = serde_json::to_vec(&Head { part: part.to_string_lossy().into_owned(), time_bases: time_bases.to_vec(), planned }).map_err(|e| e.to_string())?;
    let mut bytes = Vec::with_capacity(12 + head.len() + planned * RECORD);
    bytes.extend_from_slice(MAGIC);
    bytes.extend_from_slice(&(head.len() as u32).to_le_bytes());
    bytes.extend_from_slice(&head);
    for r in plan.unwrap_or_default() {
        bytes.extend_from_slice(&encode(r));
    }
    if plan.is_none() {
        bytes.extend_from_slice(&encode(&Record { stream: MARKER, key: false, size: 0, offset: 0, pts: 0, dur: 0 }));
    }
    let mut file = std::fs::File::create(&idx).map_err(|e| format!("create {}: {e}", idx.display()))?;
    file.write_all(&bytes).and_then(|()| file.sync_data()).map_err(|e| format!("write {}: {e}", idx.display()))?;
    let plan_map = plan.unwrap_or_default().iter().map(|r| (r.offset, r.size)).collect();
    let plan_time = started.elapsed();
    Ok(Writer { file, idx, hdr, plan: plan_map, live: plan.is_none(), error: None, plan_time, busy: plan_time })
}

impl Writer {
    /// Whether packet `i`, written to `r`, needs its record written through: then the caller
    /// hands the packet to Windows first (`mux_index::hand_over`) and calls [`Self::push`].
    pub(crate) fn needs_record(&self, i: usize, r: &Record) -> bool {
        self.live || self.plan.get(i) != Some(&(r.offset, r.size))
    }

    /// Packet `i` was written to `r` (and, when [`Self::needs_record`], handed to Windows).
    pub(crate) fn push(&mut self, i: usize, r: Record) {
        if self.error.is_some() || !self.needs_record(i, &r) {
            return;
        }
        let at = Instant::now();
        let mut out = Vec::with_capacity(2 * RECORD);
        if !self.live {
            // The plan holds for records 0..i; from here every record is written through.
            out.extend_from_slice(&encode(&Record { stream: MARKER, key: false, size: 0, offset: i as u64, pts: 0, dur: 0 }));
            self.live = true;
            self.plan.clear();
        }
        out.extend_from_slice(&encode(&r));
        if let Err(e) = self.file.write_all(&out) {
            self.error = Some(format!("write {}: {e}", self.idx.display()));
        }
        self.busy += at.elapsed();
    }

    /// Flush to disk once. What it cost, or the error.
    pub(crate) fn finish(mut self) -> Result<Cost, String> {
        let at = Instant::now();
        if let Some(e) = self.error.take() {
            return Err(e);
        }
        self.file.sync_data().map_err(|e| format!("write {}: {e}", self.idx.display()))?;
        self.busy += at.elapsed();
        let size = |p: &Path| std::fs::metadata(p).map(|m| m.len()).unwrap_or(0);
        Ok(Cost { bytes: size(&self.idx) + size(&self.hdr), ms: self.busy.as_secs_f64() * 1000.0, plan_ms: self.plan_time.as_secs_f64() * 1000.0 })
    }
}

/// The index at `base`: its head and the records to use, in file order (the plan up to its
/// marker, then the records written through).
pub(crate) fn read(base: &Path) -> Result<(Head, Vec<Record>), String> {
    let idx = idx_of(base);
    let mut bytes = Vec::new();
    std::fs::File::open(&idx).and_then(|mut f| f.read_to_end(&mut bytes)).map_err(|e| format!("read {}: {e}", idx.display()))?;
    let bad = || format!("{} is not a packet index", idx.display());
    if bytes.get(..8) != Some(&MAGIC[..]) {
        return Err(bad());
    }
    let head_len = bytes.get(8..12).map(|b| u32::from_le_bytes(b.try_into().unwrap_or_default()) as usize).ok_or_else(bad)?;
    let head: Head = serde_json::from_slice(bytes.get(12..12 + head_len).ok_or_else(bad)?).map_err(|_| bad())?;
    let all: Vec<Record> = bytes[12 + head_len..].chunks_exact(RECORD).map(decode).collect();
    let (plan, rest) = all.split_at(head.planned.min(all.len()));
    let mut records = plan.to_vec();
    for r in rest {
        if r.stream == MARKER {
            records.truncate(r.offset as usize);
        } else {
            records.push(*r);
        }
    }
    Ok((head, records))
}
