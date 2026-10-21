//! CPU-only mux integration: libx264 stands in for NVENC with the same
//! stream-copy shape (µs time base, bf=0, global header). No GPU work.

use super::*;
use crate::encoder::saver::{part_path, write_now};
use crate::ring::PacketRing;
use ff::Rescale;
use std::sync::Arc;

const US: ff::Rational = ff::Rational(1, 1_000_000);

fn open_x264(w: u32, h: u32, gop: u32) -> Option<ff::encoder::Video> {
    let codec = ff::encoder::find_by_name("libx264")?;
    let mut ctx = ff::codec::context::Context::new_with_codec(codec)
        .encoder()
        .video()
        .ok()?;
    ctx.set_width(w);
    ctx.set_height(h);
    ctx.set_format(ff::format::Pixel::YUV420P);
    ctx.set_time_base(US);
    ctx.set_frame_rate(Some(ff::Rational(60, 1)));
    ctx.set_gop(gop);
    ctx.set_max_b_frames(0);
    ctx.set_bit_rate(500_000);
    ctx.set_flags(ff::codec::Flags::GLOBAL_HEADER);
    let mut opts = ff::Dictionary::new();
    opts.set("preset", "ultrafast");
    opts.set("tune", "zerolatency");
    opts.set("bf", "0");
    Some(ctx.open_with(opts).expect("open libx264"))
}

/// Same rule as `Enc::drain`: the next PTS refines the previous duration.
fn drain_into(enc: &mut ff::encoder::Video, ring: &mut PacketRing) {
    let mut pkt = ff::Packet::empty();
    while enc.receive_packet(&mut pkt).is_ok() {
        let Some(data) = pkt.data() else { continue };
        let pts = pkt.pts().unwrap_or(0).max(0) as u64;
        if let Some(prev) = ring.last_pts_us() {
            if pts > prev {
                ring.set_last_duration_us(pts - prev);
            }
        }
        ring.push(EncodedPacket {
            frame_idx: pts,
            duration_us: 16_666,
            keyframe: pkt.is_key(),
            data: Arc::from(data),
        });
    }
}

fn temp_clip(tag: &str) -> (std::path::PathBuf, String) {
    let dir = std::env::temp_dir().join(format!("recall-mux-{tag}-{}", uuid::Uuid::new_v4().simple()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("clip.mp4").to_string_lossy().into_owned();
    (dir, path)
}

fn pcm(name: &'static str, secs: f64, tone: bool) -> AudioPcm {
    let rate = 48_000u32;
    let n = (secs * rate as f64) as usize * 2;
    let samples = (0..n)
        .map(|i| if tone { ((i / 2) as f32 * 0.05).sin() * 0.2 } else { 0.0 })
        .collect();
    AudioPcm {
        name,
        rate,
        channels: 2,
        samples,
    }
}

/// Encode a VFR libx264 stream into a ring and return its save window.
fn x264_window() -> Option<(Vec<EncodedPacket>, OwnedParams)> {
    ff::init().unwrap();
    let mut enc = open_x264(64, 64, 4)?;
    let params = OwnedParams::from_encoder(&enc);
    let mut ring = PacketRing::new(10_000_000);
    let pts_list = [
        0u64, 16_000, 50_000, 66_000, 120_000, 137_000, 200_000, 216_000, 300_000, 316_000,
        333_000, 400_000,
    ];
    for (i, &pts) in pts_list.iter().enumerate() {
        let mut vf = ff::frame::Video::new(ff::format::Pixel::YUV420P, 64, 64);
        let stride = vf.stride(0);
        for y in 0..64 {
            for x in 0..64 {
                vf.data_mut(0)[y * stride + x] = ((x + y + i * 7) & 0xff) as u8;
            }
        }
        vf.data_mut(1).fill(128);
        vf.data_mut(2).fill(128);
        vf.set_pts(Some(pts as i64));
        enc.send_frame(&vf).unwrap();
        drain_into(&mut enc, &mut ring);
    }
    enc.send_eof().unwrap();
    drain_into(&mut enc, &mut ring);
    assert_eq!(ring.span_frames(), pts_list.len() as u64);
    Some((ring.save_window_at(250_000, 420_000), params))
}

fn window_job(path: &str, packets: Vec<EncodedPacket>, params: OwnedParams, secs: f64) -> SaveJob {
    SaveJob {
        path: path.to_string(),
        seconds: 1,
        requested: Instant::now(),
        secs,
        video: VideoSrc::Window { packets, params },
        backend: "libx264",
        audio: vec![pcm("Game", secs, true), pcm("Mic", secs, false)],
        thumb: None,
        width: 64,
        height: 64,
        fps: 60,
        session_id: "test".into(),
        timing: Timing::default(),
    }
}

#[test]
fn cpu_window_muxes_to_mp4_with_vfr_timing_and_two_aac_tracks() {
    let Some((window, params)) = x264_window() else {
        eprintln!("libx264 missing in this FFmpeg build; skipped");
        return;
    };
    assert!(window[0].keyframe && pts_strictly_increase(&window));
    let secs = window_secs(&window);
    let (dir, path) = temp_clip("ok");
    let muxed = write_now(&window_job(&path, window.clone(), params, secs)).unwrap();
    assert_eq!(muxed.frames, window.len() as u64);
    assert!((muxed.secs as f64 - secs).abs() < 1e-6);
    assert!(!part_path(&path).exists(), ".part must be renamed away");

    let mut ictx = ff::format::input(&path).unwrap();
    let streams: Vec<(ff::codec::Id, ff::Rational)> = ictx
        .streams()
        .map(|s| (s.parameters().id(), s.time_base()))
        .collect();
    let ids: Vec<ff::codec::Id> = streams.iter().map(|s| s.0).collect();
    assert_eq!(ids, vec![ff::codec::Id::H264, ff::codec::Id::AAC, ff::codec::Id::AAC]);
    let mut video = Vec::new();
    let mut audio = [0usize; 2];
    for (stream, pkt) in ictx.packets() {
        let tb = streams[stream.index()].1;
        if stream.index() == 0 {
            let pts = pkt.pts().unwrap().rescale(tb, US) as u64;
            let duration = pkt.duration().rescale(tb, US) as u64;
            video.push((pts, duration, pkt.is_key()));
        } else {
            audio[stream.index() - 1] += 1;
        }
    }
    let expected: Vec<(u64, u64, bool)> = window
        .iter()
        .map(|p| (p.frame_idx, p.duration_us, p.keyframe))
        .collect();
    assert_eq!(video, expected, "VFR pts/durations/keyframes must survive the mux");
    // ~secs of 1024-sample AAC frames per track (plus encoder priming).
    let min_frames = (secs * 48_000.0 / 1024.0) as usize;
    assert!(audio.iter().all(|&n| n >= min_frames), "{audio:?} < {min_frames}");
    drop(ictx);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn audio_video_length_mismatch_fails_without_leaving_files() {
    let Some((window, params)) = x264_window() else {
        eprintln!("libx264 missing in this FFmpeg build; skipped");
        return;
    };
    let (dir, path) = temp_clip("mismatch");
    let mut job = window_job(&path, window, params, 0.3);
    job.secs = 9.0;
    let err = write_now(&job).err().expect("mismatch must fail");
    assert!(err.contains("does not match"), "{err}");
    assert!(!std::path::Path::new(&path).exists() && !part_path(&path).exists());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn pts_order_check_rejects_repeats_and_regressions() {
    let p = |pts| EncodedPacket {
        frame_idx: pts,
        duration_us: 1,
        keyframe: true,
        data: Arc::from(vec![0u8]),
    };
    assert!(pts_strictly_increase(&[]));
    assert!(pts_strictly_increase(&[p(0), p(5), p(9)]));
    assert!(!pts_strictly_increase(&[p(0), p(5), p(5)]));
    assert!(!pts_strictly_increase(&[p(0), p(9), p(5)]));
}

#[test]
fn thumbnail_is_a_jpeg_beside_the_clip() {
    ff::init().unwrap();
    let (dir, path) = temp_clip("thumb");
    let rgba: Vec<u8> = (0..64 * 48).flat_map(|i| [(i % 251) as u8, 40, 200, 255]).collect();
    let thumb = write_thumbnail(&path, &(64, 48, Arc::new(rgba))).unwrap();
    let bytes = std::fs::read(&thumb).unwrap();
    assert_eq!(&bytes[..2], &[0xFF, 0xD8], "not a JPEG");
    assert!(thumb.contains("thumbs") && thumb.ends_with("-thumb.jpg"), "{thumb}");
    let _ = std::fs::remove_dir_all(&dir);
}

/// CPU cost of a save shaped like the live 119.146 s clip (7124 packets,
/// ~363 MB, two 48 kHz stereo tracks) through the real window -> AAC -> mux
/// -> rename path. Writes ~363 MB under `<workspace>/.tmp/save-bench-*` (L:,
/// deleted after). Ignored by default:
/// `cargo test -p recall bench_ -- --ignored --nocapture --test-threads=1`.
#[test]
#[ignore]
fn bench_live_shaped_save_cpu_cost() {
    ff::init().unwrap();
    let Some(enc) = open_x264(64, 64, 60) else {
        eprintln!("libx264 missing; skipped");
        return;
    };
    let params = OwnedParams::from_encoder(&enc);
    let (count, total_bytes) = (7124usize, 363_691_446usize);
    let mut ring = PacketRing::new(120_000_000);
    for i in 0..count {
        let key = i % 60 == 0;
        // Annex-B NAL (IDR or non-IDR slice) without inner start codes, so the
        // muxer's NAL scan + length-prefix conversion does real work.
        let mut data = vec![0xAAu8; total_bytes / count];
        data[..5].copy_from_slice(&[0, 0, 0, 1, if key { 0x65 } else { 0x41 }]);
        ring.push(EncodedPacket {
            frame_idx: i as u64 * 16_725,
            duration_us: 16_725,
            keyframe: key,
            data: Arc::from(data),
        });
    }
    let started = Instant::now();
    let window = ring.save_window_at(119_146_000, count as u64 * 16_725);
    let window_ms = ms(started.elapsed());
    let secs = window_secs(&window);
    let bench_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join(".tmp")
        .join(format!("save-bench-{}", uuid::Uuid::new_v4().simple()));
    std::fs::create_dir_all(&bench_dir).unwrap();
    let path = bench_dir.join("bench.mp4").to_string_lossy().into_owned();
    let job = window_job(&path, window, params, secs);
    let mut timing = Timing::default();
    let started = Instant::now();
    let muxed = crate::encoder::saver::write_clip(
        &part_path(&path),
        &job,
        &mut produce,
        &AtomicBool::new(false),
        &mut timing,
    )
    .unwrap();
    let total_ms = ms(started.elapsed());
    let bytes = std::fs::metadata(&path).unwrap().len();
    eprintln!(
        "BENCH save: secs={:.3} packets={} bytes={bytes} window(encoder thread)={window_ms:.2}ms | \
         saver aac={:.1}ms mux={:.1}ms trailer={:.1}ms rename={:.1}ms total={total_ms:.1}ms",
        muxed.secs, muxed.frames, timing.aac_ms, timing.mux_ms, timing.trailer_ms, timing.rename_ms
    );
    assert_eq!(muxed.frames, count as u64);
    let _ = std::fs::remove_dir_all(&bench_dir);
}
