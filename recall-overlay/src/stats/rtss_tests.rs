//! Synthetic `RTSSSharedMemoryV2` buffers built to the SDK header's offsets.

use super::*;

/// Real v2.x sizes from MSVC `offsetof`/`sizeof` over RTSSSharedMemory.h.
const ENTRY: usize = 12_416;
const ARRAY_AT: usize = 2_396_256;

struct Buf(Vec<u8>);

impl Buf {
    fn new(version: u32, apps: usize) -> Self {
        let mut buf = Buf(vec![0; ARRAY_AT + apps * ENTRY]);
        buf.put(H_SIGNATURE, SIGNATURE);
        buf.put(H_VERSION, version);
        buf.put(H_APP_ENTRY_SIZE, ENTRY as u32);
        buf.put(H_APP_ARR_OFFSET, ARRAY_AT as u32);
        buf.put(H_APP_ARR_SIZE, apps as u32);
        buf
    }

    fn put(&mut self, offset: usize, value: u32) {
        self.0[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
    }

    fn app(&mut self, slot: usize, pid: u32, time0: u32, time1: u32, frames: u32) -> usize {
        let base = ARRAY_AT + slot * ENTRY;
        self.put(base + E_PROCESS_ID, pid);
        self.put(base + E_TIME0, time0);
        self.put(base + E_TIME1, time1);
        self.put(base + E_FRAMES, frames);
        base
    }

    fn frame_times(&mut self, base: usize, times: &[u32]) {
        for (i, t) in times.iter().enumerate() {
            self.put(base + E_FRAME_TIME_BUF + i * 4, *t);
        }
    }
}

fn frames(read: RtssRead) -> FrameStats {
    match read {
        RtssRead::Frames(stats) => stats,
        other => panic!("expected frames, got {other:?}"),
    }
}

#[test]
fn signature_is_the_c_multichar_rtss() {
    assert_eq!(&SIGNATURE.to_le_bytes(), b"SSTR", "'RTSS' stored as a little-endian DWORD");
}

#[test]
fn per_second_fps_for_the_target_pid() {
    let mut buf = Buf::new(0x0002_0015, 4);
    buf.app(0, 111, 1_000, 2_000, 60);
    buf.app(2, 4242, 10_000, 11_000, 144);
    let stats = frames(parse(buf.0.as_slice(), Some(4242), 11_500));
    assert_eq!(stats.fps, Some(144.0));
    assert_eq!(parse(buf.0.as_slice(), Some(999), 11_500), RtssRead::NoEntry);
}

#[test]
fn stale_entries_report_no_framerate() {
    let mut buf = Buf::new(0x0002_0015, 1);
    buf.app(0, 7, 10_000, 11_000, 144);
    let stats = frames(parse(buf.0.as_slice(), Some(7), 11_000 + STALE_MS + 1));
    assert_eq!((stats.fps, stats.low1), (None, None));
    // GetTickCount wraps every 49.7 days; a frame just before the wrap is still fresh.
    let mut wrap = Buf::new(0x0002_0015, 1);
    wrap.app(0, 7, u32::MAX - 1_500, u32::MAX - 500, 100);
    assert_eq!(frames(parse(wrap.0.as_slice(), Some(7), 300)).fps, Some(100.0));
}

#[test]
fn one_percent_low_from_frame_times_without_rtss_statistics() {
    let mut buf = Buf::new(0x0002_0015, 1);
    let base = buf.app(0, 7, 10_000, 11_000, 200);
    // 990 frames at 5 ms (200 FPS) and 10 slow frames at 20 ms (50 FPS).
    let mut times = vec![5_000u32; 990];
    times.extend([20_000u32; 10]);
    buf.frame_times(base, &times);
    let stats = frames(parse(buf.0.as_slice(), Some(7), 11_100));
    assert_eq!(stats.low_source, LowSource::FrameTimes);
    // 1000 frames → index 10 of the slowest-first list = the first 5 ms frame.
    assert_eq!(stats.low1, Some(200.0));
    times.extend([25_000u32; 2]); // now 12 slow frames: the boundary frame is slow
    buf.frame_times(base, &times);
    assert_eq!(frames(parse(buf.0.as_slice(), Some(7), 11_100)).low1, Some(50.0));
}

#[test]
fn rtss_statistics_field_wins_while_rtss_records_statistics() {
    let mut buf = Buf::new(0x0002_0015, 1);
    let base = buf.app(0, 7, 10_000, 11_000, 200);
    buf.frame_times(base, &[5_000; 1024]);
    buf.put(base + E_STAT_COUNT, 12);
    buf.put(base + E_LOW_1_PERCENT, 987); // tenths of FPS
    let stats = frames(parse(buf.0.as_slice(), Some(7), 11_100));
    assert_eq!((stats.low1, stats.low_source), (Some(98.7), LowSource::RtssStat));
    // Before v2.13 the field does not exist: fall back to the frame times.
    let mut old = Buf::new(0x0002_000C, 1);
    let base = old.app(0, 7, 10_000, 11_000, 200);
    old.frame_times(base, &[5_000; 1024]);
    old.put(base + E_STAT_COUNT, 12);
    old.put(base + E_LOW_1_PERCENT, 987);
    assert_eq!(frames(parse(old.0.as_slice(), Some(7), 11_100)).low_source, LowSource::FrameTimes);
}

#[test]
fn invalid_or_short_memory_is_not_ready() {
    let mut dead = Buf::new(0x0002_0015, 1);
    dead.put(H_SIGNATURE, 0xDEAD);
    assert_eq!(parse(dead.0.as_slice(), Some(7), 0), RtssRead::NotReady);
    assert_eq!(parse(&[0u8; 16][..], Some(7), 0), RtssRead::NotReady);
    let v1 = Buf::new(0x0001_0003, 1);
    assert_eq!(parse(v1.0.as_slice(), Some(7), 0), RtssRead::NotReady);
    // An array that claims more apps than the mapping holds is cut at the end.
    let mut lying = Buf::new(0x0002_0015, 1);
    lying.put(H_APP_ARR_SIZE, 256);
    lying.app(0, 7, 10_000, 11_000, 30);
    assert_eq!(frames(parse(lying.0.as_slice(), Some(7), 11_000)).fps, Some(30.0));
    assert_eq!(parse(lying.0.as_slice(), Some(8), 11_000), RtssRead::NoEntry);
}

#[test]
fn without_a_pid_the_last_foreground_app_is_used() {
    let mut buf = Buf::new(0x0002_0015, 2);
    buf.app(1, 55, 10_000, 11_000, 90);
    buf.put(H_LAST_FOREGROUND_PID, 55);
    assert_eq!(frames(parse(buf.0.as_slice(), None, 11_000)).fps, Some(90.0));
    buf.put(H_LAST_FOREGROUND_PID, 0);
    assert_eq!(parse(buf.0.as_slice(), None, 11_000), RtssRead::NoEntry);
}

#[test]
fn low_percentile_needs_enough_frames() {
    assert_eq!(low_1_percent(&[0; 1024]), None);
    assert_eq!(low_1_percent(&[16_667; 5]), None);
    let low = low_1_percent(&[16_667; 100]).unwrap();
    assert!((low - 60.0).abs() < 0.01);
}
