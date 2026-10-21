//! Fragmented-MP4 sink: the write-batching policy (pure), then CPU-only
//! libx264 round trips through the real muxer (finished and abandoned files
//! must both open and play). No GPU work.

use super::*;
use std::sync::{Arc, Mutex};

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

/// (pts µs, keyframe, payload) for `n` frames at ~60 fps.
fn encode_frames(enc: &mut ff::encoder::Video, n: usize) -> Vec<(u64, bool, Vec<u8>)> {
    let mut out = Vec::new();
    let mut pkt = ff::Packet::empty();
    for i in 0..n {
        let mut vf = ff::frame::Video::new(ff::format::Pixel::YUV420P, 64, 64);
        let stride = vf.stride(0);
        for y in 0..64 {
            for x in 0..64 {
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
    vec![TrackInfo { name: "Game", rate: 48_000, channels: 2 }, TrackInfo { name: "Mic", rate: 48_000, channels: 2 }]
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
