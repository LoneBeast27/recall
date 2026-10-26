//! RTSS shared memory (`RTSSSharedMemoryV2`), opened read-only.
//!
//! Layout: `RTSSSharedMemory.h` (RTSS SDK, v2.x). Offsets were checked with
//! MSVC `offsetof` against that header (app entry = 12416 bytes). Units follow
//! the SDK's own OverlayEditor plugin: frame times are microseconds, the
//! `dwStat*` framerates are tenths of FPS and only valid while RTSS records
//! statistics (`dwStatCount != 0`); otherwise 1% low is computed here from the
//! 1024-frame `dwStatFrameTimeBuf` ring (the SDK sample's linear percentile).

use serde::Serialize;

/// `'RTSS'` as a C multi-character constant.
pub const SIGNATURE: u32 = 0x5254_5353;

// Header (RTSS_SHARED_MEMORY).
const H_SIGNATURE: usize = 0;
const H_VERSION: usize = 4;
const H_APP_ENTRY_SIZE: usize = 8;
const H_APP_ARR_OFFSET: usize = 12;
const H_APP_ARR_SIZE: usize = 16;
const H_LAST_FOREGROUND_PID: usize = 68; // v2.16+

// App entry (RTSS_SHARED_MEMORY_APP_ENTRY).
const E_PROCESS_ID: usize = 0;
const E_TIME0: usize = 268;
const E_TIME1: usize = 272;
const E_FRAMES: usize = 276;
const E_STAT_COUNT: usize = 300;
const E_FRAME_TIME_BUF: usize = 924; // DWORD[1024], microseconds
const E_FRAME_TIME_BUF_LEN: usize = 1024;
const E_LOW_1_PERCENT: usize = 9176; // v2.13+, tenths of FPS

/// The entry must reach past the frame-time ring for us to use it.
const MIN_ENTRY: usize = E_FRAME_TIME_BUF + E_FRAME_TIME_BUF_LEN * 4;
const V2: u32 = 0x0002_0000;
const V2_13: u32 = 0x0002_000D;
const V2_16: u32 = 0x0002_0010;
/// RTSS's own staleness rule: no frames for 2 s → no framerate.
const STALE_MS: u32 = 2_000;
const MAX_APPS: u32 = 1_024;

/// Aligned little-endian `u32` reads from shared memory (or a test buffer).
pub trait Mem {
    fn len(&self) -> usize;
    /// The `u32` at `offset`, or `None` past the end.
    fn u32_at(&self, offset: usize) -> Option<u32>;
}

impl Mem for [u8] {
    fn len(&self) -> usize {
        <[u8]>::len(self)
    }

    fn u32_at(&self, offset: usize) -> Option<u32> {
        let bytes = self.get(offset..offset.checked_add(4)?)?;
        Some(u32::from_le_bytes(bytes.try_into().ok()?))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LowSource {
    /// `dwStatFramerate1Dot0PercentLow` (RTSS is recording statistics).
    RtssStat,
    /// Computed from the last 1024 frame times.
    FrameTimes,
    None,
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
pub struct FrameStats {
    pub fps: Option<f32>,
    pub low1: Option<f32>,
    pub low_source: LowSource,
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
#[serde(rename_all = "snake_case", tag = "read")]
pub enum RtssRead {
    /// No `RTSSSharedMemoryV2` mapping (RTSS is not running).
    NoMemory,
    /// The mapping exists but is not (or no longer) valid v2 data.
    NotReady,
    /// RTSS has no entry for the process.
    NoEntry,
    Frames(FrameStats),
}

/// A valid v2 header.
struct Header {
    version: u32,
    entry_size: usize,
    array_offset: usize,
    array_len: usize,
}

fn header<M: Mem + ?Sized>(mem: &M) -> Option<Header> {
    let at = |offset| mem.u32_at(offset);
    if at(H_SIGNATURE)? != SIGNATURE || at(H_VERSION)? < V2 {
        return None;
    }
    let entry_size = at(H_APP_ENTRY_SIZE)? as usize;
    (entry_size >= MIN_ENTRY).then_some(Header {
        version: at(H_VERSION)?,
        entry_size,
        array_offset: at(H_APP_ARR_OFFSET)? as usize,
        array_len: at(H_APP_ARR_SIZE)?.min(MAX_APPS) as usize,
    })
}

/// Byte offset of `pid`'s app entry (entries past the mapping's end are ignored).
fn find_entry<M: Mem + ?Sized>(mem: &M, h: &Header, pid: u32) -> Option<usize> {
    (0..h.array_len)
        .filter_map(|index| h.array_offset.checked_add(index.checked_mul(h.entry_size)?))
        .take_while(|&base| base.checked_add(MIN_ENTRY).is_some_and(|end| end <= mem.len()))
        .find(|&base| mem.u32_at(base + E_PROCESS_ID) == Some(pid))
}

/// Reads the framerate for `pid` (or, without a pid, RTSS's last foreground
/// app). `now_ms` is `GetTickCount()`, the clock RTSS stamps frames with.
pub fn parse<M: Mem + ?Sized>(mem: &M, pid: Option<u32>, now_ms: u32) -> RtssRead {
    let Some(h) = header(mem) else { return RtssRead::NotReady };
    let wanted = match pid {
        Some(pid) => pid,
        None if h.version >= V2_16 => match mem.u32_at(H_LAST_FOREGROUND_PID) {
            Some(pid) if pid != 0 => pid,
            _ => return RtssRead::NoEntry,
        },
        None => return RtssRead::NoEntry,
    };
    match find_entry(mem, &h, wanted) {
        Some(base) => RtssRead::Frames(entry_stats(mem, base, h.version, h.entry_size, now_ms)),
        None => RtssRead::NoEntry,
    }
}

/// `STATFLAG_RECORD`: RTSS collects the `dwStat*` statistics (1% low among
/// them) for an app only while this bit is set in its `dwStatFlags`.
pub const STATFLAG_RECORD: u32 = 0x1;
const E_STAT_FLAGS: usize = 284;

/// One poll's change to `STATFLAG_RECORD` (byte offsets of `dwStatFlags`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RecordPlan {
    /// Clear the bit here: the entry we set it on, no longer wanted.
    pub clear: Option<usize>,
    /// Set the bit here: the wanted game's entry, bit currently off.
    pub set: Option<usize>,
    /// The pid whose bit we own after applying (we only ever clear our own).
    pub owned: Option<u32>,
}

/// Recording follows `wanted` (the stats target's pid; `None` = stats off).
/// A bit someone else set (Afterburner's benchmark) is used but never owned or
/// cleared; a bit we own that went missing (RTSS restarted) is set again.
pub fn record_plan<M: Mem + ?Sized>(mem: &M, wanted: Option<u32>, owned: Option<u32>) -> RecordPlan {
    let mut plan = RecordPlan { owned, ..RecordPlan::default() };
    let Some(h) = header(mem) else { return plan };
    let flags = |pid| find_entry(mem, &h, pid).map(|base| base + E_STAT_FLAGS);
    let recording = |at: usize| mem.u32_at(at).is_some_and(|f| f & STATFLAG_RECORD != 0);
    if let Some(old) = owned.filter(|&old| Some(old) != wanted) {
        plan.clear = flags(old).filter(|&at| recording(at));
        plan.owned = None;
    }
    if let Some(at) = wanted.and_then(flags) {
        if !recording(at) {
            plan.set = Some(at);
            plan.owned = wanted;
        }
    }
    plan
}

/// How the overlay reaches RTSS's mapping. RTSS runs elevated, so a
/// non-elevated overlay may be refused write access.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Access {
    /// Read + write: we can turn on RTSS's statistics for the game.
    Write,
    /// Write refused: read only; 1% low comes from the frame-time ring.
    ReadOnly,
}

/// The fallback choice after trying a write open, then a read open.
pub fn choose_access(write_opened: bool, read_opened: bool) -> Option<Access> {
    match (write_opened, read_opened) {
        (true, _) => Some(Access::Write),
        (false, true) => Some(Access::ReadOnly),
        (false, false) => None, // no mapping: RTSS is not running
    }
}

fn entry_stats<M: Mem + ?Sized>(mem: &M, base: usize, version: u32, entry_size: usize, now_ms: u32) -> FrameStats {
    let at = |offset: usize| mem.u32_at(base + offset).unwrap_or(0);
    let (time0, time1, frames) = (at(E_TIME0), at(E_TIME1), at(E_FRAMES));
    let fresh = time1 != 0 && now_ms.wrapping_sub(time1) <= STALE_MS;
    if !fresh {
        return FrameStats { fps: None, low1: None, low_source: LowSource::None };
    }
    // The header's once-per-second formula: 1000 * dwFrames / (dwTime1 - dwTime0).
    let span = time1.wrapping_sub(time0);
    let fps = (time0 != 0 && span > 0 && span < 60_000).then(|| 1000.0 * frames as f32 / span as f32);
    let stat_low = (version >= V2_13 && entry_size > E_LOW_1_PERCENT + 3 && at(E_STAT_COUNT) != 0)
        .then(|| at(E_LOW_1_PERCENT))
        .filter(|&tenths| tenths != 0);
    if let Some(tenths) = stat_low {
        return FrameStats { fps, low1: Some(tenths as f32 / 10.0), low_source: LowSource::RtssStat };
    }
    let times: Vec<u32> = (0..E_FRAME_TIME_BUF_LEN).map(|i| at(E_FRAME_TIME_BUF + i * 4)).collect();
    match low_1_percent(&times) {
        Some(low) => FrameStats { fps, low1: Some(low), low_source: LowSource::FrameTimes },
        None => FrameStats { fps, low1: None, low_source: LowSource::None },
    }
}

/// 1% low FPS from frame times in microseconds (zeros = unfilled slots): the
/// frame time on the boundary of the slowest 1% of frames, inverted.
pub fn low_1_percent(frame_times_us: &[u32]) -> Option<f32> {
    let mut times: Vec<u32> = frame_times_us.iter().copied().filter(|&t| t > 0).collect();
    if times.len() < 10 {
        return None;
    }
    times.sort_unstable_by(|a, b| b.cmp(a));
    let index = (times.len() / 100).min(times.len() - 1);
    Some(1_000_000.0 / times[index] as f32)
}

#[cfg(test)]
#[path = "rtss_tests.rs"]
mod tests;
