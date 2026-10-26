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

/// Reads the framerate for `pid` (or, without a pid, RTSS's last foreground
/// app). `now_ms` is `GetTickCount()`, the clock RTSS stamps frames with.
pub fn parse<M: Mem + ?Sized>(mem: &M, pid: Option<u32>, now_ms: u32) -> RtssRead {
    let header = |offset| mem.u32_at(offset);
    let (Some(signature), Some(version)) = (header(H_SIGNATURE), header(H_VERSION)) else {
        return RtssRead::NotReady;
    };
    if signature != SIGNATURE || version < V2 {
        return RtssRead::NotReady;
    }
    let (Some(entry_size), Some(array_offset), Some(array_len)) =
        (header(H_APP_ENTRY_SIZE), header(H_APP_ARR_OFFSET), header(H_APP_ARR_SIZE))
    else {
        return RtssRead::NotReady;
    };
    let entry_size = entry_size as usize;
    if entry_size < MIN_ENTRY {
        return RtssRead::NotReady;
    }
    let wanted = match pid {
        Some(pid) => pid,
        None if version >= V2_16 => match header(H_LAST_FOREGROUND_PID) {
            Some(pid) if pid != 0 => pid,
            _ => return RtssRead::NoEntry,
        },
        None => return RtssRead::NoEntry,
    };
    let entry = (0..array_len.min(MAX_APPS) as usize)
        .filter_map(|index| (array_offset as usize).checked_add(index.checked_mul(entry_size)?))
        .take_while(|&base| base.checked_add(MIN_ENTRY).is_some_and(|end| end <= mem.len()))
        .find(|&base| mem.u32_at(base + E_PROCESS_ID) == Some(wanted));
    match entry {
        Some(base) => RtssRead::Frames(entry_stats(mem, base, version, entry_size, now_ms)),
        None => RtssRead::NoEntry,
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

/// Live read of RTSS's mapping (opened and closed per call, like the SDK's
/// own readers, so a restarted RTSS is picked up).
pub fn read(pid: Option<u32>) -> RtssRead {
    use windows::core::w;
    use windows::Win32::Foundation::CloseHandle;
    use windows::Win32::System::Memory::{
        MapViewOfFile, OpenFileMappingW, UnmapViewOfFile, VirtualQuery, FILE_MAP_READ, MEMORY_BASIC_INFORMATION,
    };
    use windows::Win32::System::SystemInformation::GetTickCount;
    unsafe {
        let Ok(handle) = OpenFileMappingW(FILE_MAP_READ.0, false, w!("RTSSSharedMemoryV2")) else {
            return RtssRead::NoMemory;
        };
        let view = MapViewOfFile(handle, FILE_MAP_READ, 0, 0, 0);
        let result = if view.Value.is_null() {
            RtssRead::NoMemory
        } else {
            let mut info = MEMORY_BASIC_INFORMATION::default();
            let queried = VirtualQuery(
                Some(view.Value as *const _),
                &mut info,
                std::mem::size_of::<MEMORY_BASIC_INFORMATION>(),
            );
            let len = if queried == 0 { 0 } else { info.RegionSize };
            let mem = SharedView { base: view.Value as *const u8, len };
            let result = parse(&mem, pid, GetTickCount());
            let _ = UnmapViewOfFile(view);
            result
        };
        let _ = CloseHandle(handle);
        result
    }
}

/// A mapped view another process writes to: every read is a volatile copy.
struct SharedView {
    base: *const u8,
    len: usize,
}

impl Mem for SharedView {
    fn len(&self) -> usize {
        self.len
    }

    fn u32_at(&self, offset: usize) -> Option<u32> {
        if !offset.is_multiple_of(4) || offset.checked_add(4)? > self.len {
            return None;
        }
        // The view is page-aligned and every field offset is 4-aligned.
        Some(unsafe { std::ptr::read_volatile(self.base.add(offset).cast::<u32>()) })
    }
}

#[cfg(test)]
#[path = "rtss_tests.rs"]
mod tests;
