//! Restore policy, fail-first (`.planning/RESTORE-POLICY.md` (d) tests 1, 2, 6, 7): what the next
//! start makes of a long recording a kill or a power cut left as `.part`. Real fragmented MP4
//! through `Fmp4Sink` (libx264 on the CPU, skipped without it) or the fake sink; temp folders
//! only, no device, no audio, no window.

use super::fmp4::Fmp4Sink;
use super::machine::{Recorder, Sink};
use super::test_sink::{pkt, temp_dir, tracks, FakeFactory, MS};
use super::*;
use ffmpeg_next as ff;

const NAME: &str = "G__2026-11-10__12-00-00__rec.mp4";

struct Gone(PathBuf);
impl Drop for Gone {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn x264() -> Option<ff::encoder::Video> {
    let codec = ff::encoder::find_by_name("libx264")?;
    let mut ctx = ff::codec::context::Context::new_with_codec(codec).encoder().video().ok()?;
    ctx.set_width(64);
    ctx.set_height(64);
    ctx.set_format(ff::format::Pixel::YUV420P);
    ctx.set_time_base(ff::Rational(1, 1_000_000));
    ctx.set_frame_rate(Some(ff::Rational(60, 1)));
    ctx.set_gop(10);
    ctx.set_max_b_frames(0);
    ctx.set_bit_rate(400_000);
    ctx.set_flags(ff::codec::Flags::GLOBAL_HEADER);
    let mut opts = ff::Dictionary::new();
    opts.set("preset", "ultrafast");
    opts.set("tune", "zerolatency");
    opts.set("bf", "0");
    ctx.open_with(opts).ok()
}

/// The bytes a crash leaves: 300 frames (5 s) streamed into a real `Fmp4Sink`, never finished
/// (only whole fragments reach the disk). None without libx264.
fn crashed_recording(dir: &Path) -> Option<Vec<u8>> {
    ff::init().unwrap();
    let mut enc = x264()?;
    let params = ff::codec::Parameters::from(&enc);
    let mut frames = Vec::new();
    let mut packet = ff::Packet::empty();
    for i in 0..300usize {
        let mut vf = ff::frame::Video::new(ff::format::Pixel::YUV420P, 64, 64);
        let stride = vf.stride(0);
        for y in 0..64usize {
            for x in 0..64usize {
                vf.data_mut(0)[y * stride + x] = ((x * 3 + y + i * 9) & 0xff) as u8;
            }
        }
        vf.set_pts(Some(i as i64 * 16_667));
        enc.send_frame(&vf).unwrap();
        while enc.receive_packet(&mut packet).is_ok() {
            frames.push((packet.pts().unwrap() as u64, packet.is_key(), packet.data().unwrap().to_vec()));
        }
    }
    let part = dir.join("source.mp4.part");
    let mut sink = Fmp4Sink::create(&part, &params, &tracks()).expect("create sink");
    let tone: Vec<f32> = (0..1_600).map(|i| ((i / 2) as f32 * 0.03).sin() * 0.2).collect();
    for (i, (pts, key, data)) in frames.iter().enumerate() {
        let next = frames.get(i + 1).map_or(pts + 16_667, |f| f.0);
        sink.video(*pts, next - pts, *key, data).unwrap();
        sink.audio(0, &tone).unwrap();
        sink.silence(1, 800).unwrap();
    }
    drop(sink); // no finish(): the crash shape
    let bytes = std::fs::read(&part).unwrap();
    std::fs::remove_file(&part).unwrap();
    Some(bytes)
}

/// Where the last whole `moof`+`mdat` pair in `bytes` ends (top-level box headers only).
fn last_whole_fragment_end(bytes: &[u8]) -> usize {
    let (mut pos, mut end, mut after_moof) = (0usize, 0usize, false);
    while pos + 8 <= bytes.len() {
        let size = u32::from_be_bytes(bytes[pos..pos + 4].try_into().unwrap()) as usize;
        let kind = &bytes[pos + 4..pos + 8];
        if size < 8 || pos + size > bytes.len() {
            break;
        }
        match kind {
            b"moof" => after_moof = true,
            b"mdat" if after_moof => {
                end = pos + size;
                after_moof = false;
            }
            b"ftyp" | b"moov" => end = pos + size,
            _ => {}
        }
        pos += size;
    }
    end
}

/// (packets of every stream, packets whose payload is all zero bytes, video end in seconds).
fn demux(path: &Path) -> (usize, usize, f64) {
    let mut input = ff::format::input(&path).expect("the recovered recording opens");
    let tb = input.stream(0).unwrap().time_base();
    let (mut all, mut zero, mut end) = (0, 0, 0f64);
    for (stream, p) in input.packets() {
        all += 1;
        if p.data().is_some_and(|d| d.iter().all(|b| *b == 0)) {
            zero += 1;
        }
        if stream.index() == 0 {
            end = end.max((p.pts().unwrap_or(0) + p.duration()) as f64 * f64::from(tb));
        }
    }
    (all, zero, end)
}

/// Restore test 1: a `.part` torn mid-fragment (a kill mid-write) is cut back to its last
/// whole fragment, and nothing past that fragment is played.
#[test]
fn a_torn_recording_is_cut_back_to_its_last_whole_fragment() {
    let dir = temp_dir("restore-torn");
    let _gone = Gone(dir.clone());
    let Some(bytes) = crashed_recording(&dir) else { return eprintln!("libx264 missing; skipped") };
    let torn = &bytes[..bytes.len() * 77 / 100];
    let whole = last_whole_fragment_end(torn);
    assert!(whole < torn.len(), "the fixture is torn mid-fragment");
    let reference = dir.join("reference.mp4");
    std::fs::write(&reference, &torn[..whole]).unwrap();
    std::fs::write(dir.join(format!("{NAME}.part")), torn).unwrap();
    recover_in(&dir);
    let recovered = dir.join(NAME);
    let len = std::fs::metadata(&recovered).expect("recovered under its own name").len() as usize;
    assert_eq!(len, whole, "the torn tail past the last whole fragment is cut off: {len} B on disk, the last whole fragment ends at {whole} B");
    assert_eq!(demux(&recovered).0, demux(&reference).0, "no packet past the last whole fragment");
}

/// Restore test 2: a power cut leaves a zero-filled tail; the repaired file decodes no packet
/// made of those zero bytes (today FFmpeg returns them as garbage frames at the end).
#[test]
fn a_zero_filled_tail_is_never_played_as_frames() {
    let dir = temp_dir("restore-zeros");
    let _gone = Gone(dir.clone());
    let Some(bytes) = crashed_recording(&dir) else { return eprintln!("libx264 missing; skipped") };
    let mut cut = bytes[..bytes.len() / 2].to_vec();
    cut.extend(std::iter::repeat_n(0u8, 200_000));
    std::fs::write(dir.join(format!("{NAME}.part")), &cut).unwrap();
    recover_in(&dir);
    let (_, zero, _) = demux(&dir.join(NAME));
    assert_eq!(zero, 0, "the zero-filled tail decoded as {zero} all-zero packets in the recovered recording");
}

/// Restore test 6: the library's quick index read (`library/mp4.rs`, the same file the app
/// builds: `mvhd`, then `mvex/mehd`) gives a recovered recording its length, so the app never
/// falls back to an ffprobe scan of every fragment on the hard drive.
#[test]
fn the_library_index_read_gives_a_recovered_recording_its_length() {
    let dir = temp_dir("restore-length");
    let _gone = Gone(dir.clone());
    let Some(bytes) = crashed_recording(&dir) else { return eprintln!("libx264 missing; skipped") };
    std::fs::write(dir.join(format!("{NAME}.part")), &bytes).unwrap();
    recover_in(&dir);
    let recovered = dir.join(NAME);
    let (_, _, video_end) = demux(&recovered);
    let indexed = super::library_mp4::duration(&recovered);
    assert!(
        indexed.is_some_and(|secs| (secs - video_end).abs() < 0.05),
        "the library index read gives {indexed:?} s for a recovered recording whose video ends at {video_end:.3} s"
    );
}

/// Restore test 7: a mark pressed during a recording that a crash ended is beside the recovered
/// file (today the marks sidecar is written only at Stop).
#[test]
fn a_mark_pressed_before_a_crash_is_kept_with_the_recovered_recording() {
    let dir = temp_dir("restore-marks");
    let _gone = Gone(dir.clone());
    let path = dir.join(NAME);
    let mut rec = Recorder::new(FakeFactory::default(), tracks());
    let t0 = Instant::now();
    rec.start(path.clone(), Some((0, t0)), t0);
    rec.video(100 * MS, true, pkt(1), false, t0);
    rec.mark(600 * MS);
    rec.video(1_100 * MS, false, pkt(2), false, t0);
    rec.video(1_116 * MS, false, pkt(3), false, t0);
    assert!(naming::part_of(&path).is_file() && rec.is_active());
    drop(rec); // the engine is killed: no stop, no finish
    let results = recover_in(&dir);
    assert!(matches!(&results[..], [(_, Ok(p))] if *p == path), "{results:?}");
    let sidecar = crate::bookmarks::sidecar_path(&path);
    assert_eq!(
        std::fs::read_to_string(&sidecar).ok().as_deref(),
        Some(r#"{"version":1,"marks":[0.5]}"#),
        "the mark pressed before the crash is in {}",
        sidecar.display()
    );
}
