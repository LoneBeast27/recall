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
fn rtss_low_is_trusted_only_while_rtss_records_statistics() {
    let mut buf = Buf::new(0x0002_0015, 1);
    let base = buf.app(0, 7, 10_000, 11_000, 144);
    buf.put(base + E_LOW_1_PERCENT, 555); // left over from an earlier recording
    buf.frame_times(base, &[8_000; 500]); // 125 FPS frames, no recording
    let stats = frames(parse(buf.0.as_slice(), Some(7), 11_100));
    assert_eq!((stats.low1, stats.low_source), (Some(125.0), LowSource::FrameTimes), "dwStatCount = 0: field ignored");
    // Read-only and nothing in the frame-time ring either: FPS stays, 1% low is "—".
    let mut bare = Buf::new(0x0002_0015, 1);
    bare.app(0, 7, 10_000, 11_000, 144);
    let stats = frames(parse(bare.0.as_slice(), Some(7), 11_100));
    assert_eq!((stats.fps, stats.low1, stats.low_source), (Some(144.0), None, LowSource::None));
}

impl Buf {
    fn flags(&self, base: usize) -> u32 {
        self.0.as_slice().u32_at(base + E_STAT_FLAGS).unwrap()
    }

    /// What the live client does with a plan: atomic bit set / clear.
    fn apply(&mut self, plan: &RecordPlan) {
        if let Some(at) = plan.clear {
            let value = self.0.as_slice().u32_at(at).unwrap() & !STATFLAG_RECORD;
            self.put(at, value);
        }
        if let Some(at) = plan.set {
            let value = self.0.as_slice().u32_at(at).unwrap() | STATFLAG_RECORD;
            self.put(at, value);
        }
    }
}

#[test]
fn stats_on_switches_recording_on_for_the_game_and_owns_it() {
    let mut buf = Buf::new(0x0002_0015, 2);
    let game = buf.app(1, 42, 10_000, 11_000, 144);
    buf.put(game + E_STAT_FLAGS, 0x10); // unrelated bits survive
    let plan = record_plan(buf.0.as_slice(), Some(42), None);
    assert_eq!(plan, RecordPlan { clear: None, set: Some(game + E_STAT_FLAGS), owned: Some(42) });
    buf.apply(&plan);
    assert_eq!(buf.flags(game), 0x11);
    // Next poll: already on and ours, nothing to do.
    assert_eq!(record_plan(buf.0.as_slice(), Some(42), Some(42)), RecordPlan { owned: Some(42), ..RecordPlan::default() });
    // RTSS restarted (the entry's flags are fresh): switch it on again.
    buf.put(game + E_STAT_FLAGS, 0);
    assert_eq!(record_plan(buf.0.as_slice(), Some(42), Some(42)).set, Some(game + E_STAT_FLAGS));
}

#[test]
fn someone_elses_recording_is_used_but_never_owned_or_cleared() {
    let mut buf = Buf::new(0x0002_0015, 1);
    let game = buf.app(0, 42, 10_000, 11_000, 144);
    buf.put(game + E_STAT_FLAGS, STATFLAG_RECORD); // Afterburner's benchmark
    let plan = record_plan(buf.0.as_slice(), Some(42), None);
    assert_eq!(plan, RecordPlan::default(), "already recording: no write, not ours");
    assert_eq!(record_plan(buf.0.as_slice(), None, plan.owned), RecordPlan::default(), "stats off leaves it alone");
    assert_eq!(buf.flags(game), STATFLAG_RECORD);
}

#[test]
fn target_change_moves_recording_and_stats_off_clears_only_ours() {
    let mut buf = Buf::new(0x0002_0015, 3);
    let (old, new) = (buf.app(0, 42, 10_000, 11_000, 144), buf.app(2, 77, 10_000, 11_000, 60));
    let plan = record_plan(buf.0.as_slice(), Some(42), None);
    buf.apply(&plan);
    let plan = record_plan(buf.0.as_slice(), Some(77), plan.owned);
    assert_eq!(plan, RecordPlan { clear: Some(old + E_STAT_FLAGS), set: Some(new + E_STAT_FLAGS), owned: Some(77) });
    buf.apply(&plan);
    assert_eq!((buf.flags(old), buf.flags(new)), (0, STATFLAG_RECORD));
    let off = record_plan(buf.0.as_slice(), None, plan.owned);
    assert_eq!(off, RecordPlan { clear: Some(new + E_STAT_FLAGS), set: None, owned: None });
    buf.apply(&off);
    assert_eq!(buf.flags(new), 0);
}

#[test]
fn missing_entries_and_bad_memory_change_nothing() {
    let mut buf = Buf::new(0x0002_0015, 1);
    buf.app(0, 5, 10_000, 11_000, 30);
    assert_eq!(record_plan(buf.0.as_slice(), Some(42), None), RecordPlan::default(), "game not hooked yet");
    assert_eq!(
        record_plan(buf.0.as_slice(), Some(42), Some(42)),
        RecordPlan { owned: Some(42), ..RecordPlan::default() },
        "our game's entry briefly gone: keep ownership, retry next poll"
    );
    assert_eq!(record_plan(buf.0.as_slice(), None, Some(42)), RecordPlan::default(), "our exited game: nothing left to clear");
    assert_eq!(record_plan(&[0u8; 64][..], None, Some(42)).owned, Some(42), "invalid mapping: decide later");
}

#[test]
fn access_falls_back_to_read_only_when_write_is_refused() {
    assert_eq!(choose_access(true, true), Some(Access::Write));
    assert_eq!(choose_access(false, true), Some(Access::ReadOnly), "elevated RTSS refused our write");
    assert_eq!(choose_access(false, false), None, "no mapping: RTSS not running");
}

#[test]
fn low_percentile_needs_enough_frames() {
    assert_eq!(low_1_percent(&[0; 1024]), None);
    assert_eq!(low_1_percent(&[16_667; 5]), None);
    let low = low_1_percent(&[16_667; 100]).unwrap();
    assert!((low - 60.0).abs() < 0.01);
}
