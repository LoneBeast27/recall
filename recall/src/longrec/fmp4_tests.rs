//! Fragmented-MP4 sink: the write-batching policy (pure), then CPU-only
//! libx264 round trips through the real muxer (finished and abandoned files
//! must both open and play). No GPU work.

use super::writer::{FLUSH_AT, FLUSH_EVERY};
use super::*;
use std::io::Write;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Records the size of every `write` call that reaches the "disk".
#[derive(Clone, Default)]
struct Disk(Arc<Mutex<Vec<usize>>>);
impl Write for Disk {
    fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().push(b.len());
        Ok(b.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

const CHUNK: usize = 1 << 20;

#[test]
fn writes_are_large_and_end_on_fragment_boundaries() {
    let disk = Disk::default();
    let mut w = FileWriter::new(disk.clone(), CHUNK);
    w.push(&[0; 1000]); // header (short = flush point)
    w.flush_now();
    assert_eq!(*disk.0.lock().unwrap(), vec![1000], "header goes out at once");
    // Fragments of ~1.5 MiB arrive as one full AVIO chunk + a short tail.
    for _ in 0..5 {
        w.push(&vec![1; CHUNK]); // mid-fragment: never a boundary
        w.push(&vec![2; CHUNK / 2]); // fragment end
    }
    let writes = disk.0.lock().unwrap().clone();
    assert_eq!(writes.len(), 2, "{writes:?}");
    assert!(writes[1] >= FLUSH_AT, "batched >= 4 MiB: {writes:?}");
    assert_eq!(writes[1] % (CHUNK + CHUNK / 2), 0, "ends exactly on a fragment boundary");
    let total = w.finish().unwrap();
    assert_eq!(total, 1000 + 5 * (CHUNK + CHUNK / 2) as u64);
}

#[test]
fn a_quiet_stream_still_reaches_the_disk_every_few_seconds() {
    let disk = Disk::default();
    let mut w = FileWriter::new(disk.clone(), CHUNK);
    let t0 = Instant::now();
    w.push_at(&[0; 1000], t0); // header
    w.flush_now();
    let writes = || disk.0.lock().unwrap().clone();
    // Small fragments (a static scene): far below 4 MiB, so only the timer
    // sends them, at the first fragment boundary 5 s after the last write.
    w.push_at(&[1; 300], t0 + Duration::from_secs(2));
    w.push_at(&[2; 300], t0 + Duration::from_secs(4));
    assert_eq!(writes(), vec![1000], "under 5 s: still buffered");
    let tick = t0 + Duration::from_secs(1) + FLUSH_EVERY;
    w.push_at(&[3; 300], tick);
    assert_eq!(writes(), vec![1000, 900], "the boundary after 5 s writes everything so far");
    w.push_at(&[4; 300], tick + Duration::from_secs(1));
    assert_eq!(writes().len(), 2, "the timer restarts at each write");
    // A full-buffer callback is mid-fragment: never written by the timer.
    w.push_at(&vec![5; CHUNK], tick + FLUSH_EVERY * 2);
    assert_eq!(writes().len(), 2, "mid-fragment: waits for the boundary");
    w.push_at(&[6; 10], tick + FLUSH_EVERY * 2);
    assert_eq!(writes(), vec![1000, 900, 300 + CHUNK + 10]);
}

#[test]
fn abandon_keeps_only_complete_fragments() {
    let mut w = FileWriter::new(Vec::new(), CHUNK);
    w.push(&[7; 100]);
    w.push(&[8; 200]); // complete fragment
    w.push(&vec![9; CHUNK]); // half a fragment, cut by the full buffer
    w.abandon();
    let out = w.into_inner();
    assert_eq!(out.len(), 300);
    assert!(out.iter().all(|b| *b != 9));
}

#[test]
fn a_write_error_is_sticky() {
    struct Full;
    impl Write for Full {
        fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
            Err(std::io::Error::other("disk full"))
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut w = FileWriter::new(Full, CHUNK);
    w.push(&[0; 10]);
    w.flush_now();
    assert!(w.error().unwrap().contains("disk full"));
    w.push(&[0; 10]);
    assert!(w.finish().is_err());
    assert_eq!(w.bytes(), 0);
}

// --- real muxer, libx264 standing in for NVENC (same bf=0 µs shape) --------

fn x264(w: u32, h: u32) -> Option<ff::encoder::Video> {
    let codec = ff::encoder::find_by_name("libx264")?;
    let mut ctx = ff::codec::context::Context::new_with_codec(codec).encoder().video().ok()?;
    ctx.set_width(w);
    ctx.set_height(h);
    ctx.set_format(ff::format::Pixel::YUV420P);
    ctx.set_time_base(US);
    ctx.set_frame_rate(Some(ff::Rational(60, 1)));
    ctx.set_gop(10);
    ctx.set_max_b_frames(0);
    ctx.set_bit_rate(400_000);
    ctx.set_flags(ff::codec::Flags::GLOBAL_HEADER);
    let mut opts = ff::Dictionary::new();
    opts.set("preset", "ultrafast");
    opts.set("tune", "zerolatency");
    opts.set("bf", "0");
    Some(ctx.open_with(opts).expect("open libx264"))
}

/// (pts µs, keyframe, payload) for `n` 64x64 frames at ~60 fps.
fn encode_frames(enc: &mut ff::encoder::Video, n: usize) -> Vec<(u64, bool, Vec<u8>)> {
    encode_sized(enc, n, 64, 64)
}

/// [`encode_frames`] for a `w`x`h` encoder.
fn encode_sized(enc: &mut ff::encoder::Video, n: usize, w: u32, h: u32) -> Vec<(u64, bool, Vec<u8>)> {
    let mut out = Vec::new();
    let mut pkt = ff::Packet::empty();
    for i in 0..n {
        let mut vf = ff::frame::Video::new(ff::format::Pixel::YUV420P, w, h);
        let stride = vf.stride(0);
        for y in 0..h as usize {
            for x in 0..w as usize {
                vf.data_mut(0)[y * stride + x] = ((x * 3 + y + i * 9) & 0xff) as u8;
            }
        }
        vf.set_pts(Some(i as i64 * 16_667));
        enc.send_frame(&vf).unwrap();
        while enc.receive_packet(&mut pkt).is_ok() {
            out.push((pkt.pts().unwrap() as u64, pkt.is_key(), pkt.data().unwrap().to_vec()));
        }
    }
    enc.send_eof().unwrap();
    while enc.receive_packet(&mut pkt).is_ok() {
        out.push((pkt.pts().unwrap() as u64, pkt.is_key(), pkt.data().unwrap().to_vec()));
    }
    out
}

fn tracks() -> Vec<TrackInfo> {
    vec![TrackInfo { name: "Game", rate: 48_000, channels: 2, device: false }, TrackInfo { name: "Mic", rate: 48_000, channels: 2, device: true }]
}

fn temp_part(tag: &str) -> (std::path::PathBuf, std::path::PathBuf) {
    let dir = std::env::temp_dir().join(format!("recall-fmp4-{tag}-{}", uuid::Uuid::new_v4().simple()));
    std::fs::create_dir_all(&dir).unwrap();
    (dir.clone(), dir.join("G__2026-10-22__14-05-09__rec.mp4.part"))
}

/// (video packets, audio packets per stream, video end secs) of an MP4 on disk.
fn probe(path: &Path) -> (usize, Vec<usize>, f64) {
    let mut ictx = ff::format::input(&path).expect("recording opens");
    let streams = ictx.streams().count();
    let video_tb = ictx.stream(0).unwrap().time_base();
    let mut counts = vec![0usize; streams];
    let mut video_end = 0f64;
    for (stream, pkt) in ictx.packets() {
        counts[stream.index()] += 1;
        if stream.index() == 0 {
            let end = (pkt.pts().unwrap_or(0) + pkt.duration()) as f64 * f64::from(video_tb);
            video_end = video_end.max(end);
        }
    }
    (counts[0], counts[1..].to_vec(), video_end)
}

/// `moov/mvhd` duration in seconds (what the library reads first).
fn mvhd_secs(file: &[u8]) -> Option<f64> {
    let (moov, moov_end) = child_box(file, 0, file.len(), b"moov")?;
    let (mvhd, _) = child_box(file, moov, moov_end, b"mvhd")?;
    let wide = file[mvhd] == 1;
    let ts_at = mvhd + 4 + if wide { 16 } else { 8 };
    let timescale = u32::from_be_bytes(file[ts_at..ts_at + 4].try_into().unwrap()) as f64;
    let at = ts_at + 4;
    let units = if wide {
        u64::from_be_bytes(file[at..at + 8].try_into().unwrap())
    } else {
        u32::from_be_bytes(file[at..at + 4].try_into().unwrap()) as u64
    };
    Some(units as f64 / timescale)
}

fn write_recording(part: &Path, frames: &[(u64, bool, Vec<u8>)], params: &ff::codec::Parameters) -> Fmp4Sink {
    let mut sink = Fmp4Sink::create(part, params, &tracks()).expect("create sink");
    let tone: Vec<f32> = (0..9_600).map(|i| ((i / 2) as f32 * 0.03).sin() * 0.2).collect();
    for (i, (pts, key, data)) in frames.iter().enumerate() {
        let next = frames.get(i + 1).map(|f| f.0).unwrap_or(pts + 16_667);
        sink.video(*pts, next - pts, *key, data).unwrap();
        // ~16.7 ms of audio per frame on Game; Mic is silent padding.
        sink.audio(0, &tone[..800 * 2]).unwrap();
        sink.silence(1, 800).unwrap();
    }
    sink
}

#[test]
fn finished_recording_has_video_and_every_audio_track() {
    ff::init().unwrap();
    let Some(mut enc) = x264(64, 64) else { return eprintln!("libx264 unavailable; skipped") };
    let params = ff::codec::Parameters::from(&enc);
    let frames = encode_frames(&mut enc, 120); // 2 s, keyframe every 10 frames
    let (dir, part) = temp_part("finish");
    let mut sink = write_recording(&part, &frames, &params);
    let bytes = sink.finish().unwrap();
    drop(sink);
    assert_eq!(bytes, std::fs::metadata(&part).unwrap().len());
    let (video, audio, duration) = probe(&part);
    assert_eq!(video, frames.len(), "every video packet is in the file");
    assert_eq!(audio.len(), 2, "Game + Mic tracks");
    assert!(audio.iter().all(|n| *n >= 90), "2 s of AAC frames per track: {audio:?}");
    assert!((duration - 2.0).abs() < 0.15, "video end {duration}");
    let header = mvhd_secs(&std::fs::read(&part).unwrap()).expect("mvhd present");
    assert!((header - 2.0).abs() < 0.05, "patched movie duration {header}");
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn an_abandoned_recording_still_plays_its_complete_fragments() {
    ff::init().unwrap();
    let Some(mut enc) = x264(64, 64) else { return eprintln!("libx264 unavailable; skipped") };
    let params = ff::codec::Parameters::from(&enc);
    let frames = encode_frames(&mut enc, 95);
    let (dir, part) = temp_part("crash");
    let sink = write_recording(&part, &frames, &params);
    drop(sink); // no finish(): what a crash after the last flush leaves behind
    let (video, audio, _) = probe(&part);
    assert!(video >= 80 && video < frames.len(), "complete fragments only: {video} of {}", frames.len());
    assert_eq!(audio.len(), 2);
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn create_refuses_to_overwrite_an_existing_part() {
    ff::init().unwrap();
    let Some(enc) = x264(64, 64) else { return };
    let params = ff::codec::Parameters::from(&enc);
    let (dir, part) = temp_part("exists");
    std::fs::write(&part, b"someone else's").unwrap();
    assert!(Fmp4Sink::create(&part, &params, &tracks()).is_err());
    assert_eq!(std::fs::read(&part).unwrap(), b"someone else's");
    let _ = std::fs::remove_dir_all(dir);
}

/// Merge gate 11-07 (P2): the parts were only ever tested against a fake sink. A
/// continued recording through the real muxer: two files, each with its own size,
/// each starting on a keyframe. Written under the worktree's `.tmp`, never a clip folder.
#[test]
fn a_continued_recording_writes_two_playable_parts_through_the_real_muxer() {
    use super::super::machine::{Cause, Event, Recorder, PART_GAP};
    ff::init().unwrap();
    // A skip goes through `hw_skip` (printed past libtest's capture, counted, and a failure
    // under RECALL_REQUIRE_NVENC=1); RECALL_FAKE_NO_NVENC=1 takes this path on purpose.
    if crate::hw_skip::faked_absent() {
        return crate::hw_skip::skipped("encoders treated as absent (RECALL_FAKE_NO_NVENC)");
    }
    let (Some(mut small), Some(mut wide)) = (x264(64, 64), x264(96, 64)) else { return crate::hw_skip::skipped("libx264 unavailable") };
    let (first_params, second_params) = (ff::codec::Parameters::from(&small), ff::codec::Parameters::from(&wide));
    let first = encode_sized(&mut small, 40, 64, 64);
    let second = encode_sized(&mut wide, 40, 96, 64);
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../.tmp").join(format!("longrec-parts-{}", uuid::Uuid::new_v4().simple()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("G__2026-11-07__10-00-00__rec.mp4");
    let mut rec = Recorder::new(Fmp4Factory, tracks());
    rec.set_video(Some(first_params));
    let t0 = Instant::now();
    rec.start(path.clone(), Some((0, t0)), t0);
    let tone: Vec<f32> = (0..1_600).map(|i| ((i / 2) as f32 * 0.03).sin() * 0.2).collect();
    let feed = |rec: &mut Recorder<Fmp4Factory>, frames: &[(u64, bool, Vec<u8>)], base_us: u64, at: Instant| {
        for (pts, key, data) in frames {
            let now = at + Duration::from_micros(*pts);
            assert!(!rec.video(base_us + pts, *key, Arc::from(data.as_slice()), false, now).iter().any(|e| matches!(e, Event::Failed { .. })));
            rec.audio(0, &tone, now);
        }
    };
    feed(&mut rec, &first, 0, t0);
    let t1 = t0 + PART_GAP;
    let events = rec.encoder_reopened(t1, Cause::Size, Some(second_params));
    assert!(matches!(events.last(), Some(Event::Continued { part: 1, .. })), "{events:?}");
    feed(&mut rec, &second, PART_GAP.as_micros() as u64, t1);
    let events = rec.stop();
    assert!(matches!(&events[..], [Event::Stopped { parts: 2, .. }]), "{events:?}");
    let part2 = dir.join("G__2026-11-07__10-00-00__rec-2.mp4");
    for (file, width, sent) in [(&path, 64, first.len()), (&part2, 96, second.len())] {
        let mut ictx = ff::format::input(file).unwrap_or_else(|e| panic!("{} opens: {e}", file.display()));
        let stream = ictx.stream(0).unwrap();
        // SAFETY: the stream's parameters are valid while `ictx` lives.
        let (w, h) = unsafe { ((*stream.parameters().as_ptr()).width, (*stream.parameters().as_ptr()).height) };
        assert_eq!((w, h), (width, 64), "{} has its own encoder's size", file.display());
        let video: Vec<bool> = ictx.packets().filter(|(s, _)| s.index() == 0).map(|(_, p)| p.is_key()).collect();
        assert_eq!(video.first(), Some(&true), "{} starts on a keyframe", file.display());
        assert_eq!(video.len(), sent, "{}: every packet of its encoder", file.display());
    }
    let _ = std::fs::remove_dir_all(&dir);
}
