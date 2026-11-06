//! A real clip across an encoder gap, read back with the bundled FFmpeg: every video packet
//! decodes, the video covers the gap (the last frame before it is held), and the audio track
//! lines up with the video. Real NVENC, real MP4 mux, one fed game-audio track; skips (and
//! says so) without NVENC.

use std::sync::atomic::Ordering::Relaxed;
use std::sync::Arc;
use std::time::Duration;

use ffmpeg_next as ff;

use super::frame_fit::wobble_tests::{nvenc, Loop, BIG};
use super::EncStats;
use crate::audio::TrackRing;

/// Audio and video may differ by this much in length and start: one AAC frame (21 ms) of
/// priming / padding plus one 60 fps frame of nominal tail, with room to spare.
const SYNC_TOLERANCE_S: f64 = 0.1;

/// One 60 fps frame of 48 kHz stereo 440 Hz tone (800 sample frames).
fn tone(start: u64) -> Vec<f32> {
    (0..800u64).flat_map(|i| {
        let s = (((start + i) as f64) * 440.0 * std::f64::consts::TAU / 48_000.0).sin() as f32 * 0.25;
        [s, s]
    }).collect()
}

/// `count` CPU frames at 60 fps real time, each with its 16.7 ms of audio.
fn av_frames(enc: &mut Loop, ring: &TrackRing, fed: &mut u64, count: u64) {
    for _ in 0..count {
        enc.rgba(BIG);
        ring.push_for_test(&tone(*fed));
        *fed += 800;
        std::thread::sleep(Duration::from_millis(16));
    }
}

/// What the saved file holds, decoded.
#[derive(Debug)]
struct ReadBack {
    video_packets: u64,
    video_frames: u64,
    video_start_s: f64,
    video_secs: f64,
    audio_start_s: f64,
    audio_secs: f64,
}

fn read_back(path: &std::path::Path) -> ReadBack {
    let mut ictx = ff::format::input(&path).expect("the clip opens");
    let vi = ictx.streams().best(ff::media::Type::Video).expect("video stream").index();
    let ai = ictx.streams().best(ff::media::Type::Audio).expect("audio stream").index();
    let tb = |i: usize| f64::from(ictx.stream(i).unwrap().time_base());
    let (vtb, atb) = (tb(vi), tb(ai));
    let ctx = |i: usize| ff::codec::context::Context::from_parameters(ictx.stream(i).unwrap().parameters()).unwrap();
    let mut vdec = ctx(vi).decoder().video().expect("video decoder");
    let mut adec = ctx(ai).decoder().audio().expect("audio decoder");
    let (mut vframe, mut aframe) = (ff::frame::Video::empty(), ff::frame::Audio::empty());
    let mut rb = ReadBack { video_packets: 0, video_frames: 0, video_start_s: f64::MAX, video_secs: 0.0, audio_start_s: f64::MAX, audio_secs: 0.0 };
    let mut video_end = 0.0f64;
    let mut audio_samples = 0u64;
    let mut rate = 48_000u32;
    for (stream, packet) in ictx.packets() {
        let pts = packet.pts().unwrap_or(0) as f64;
        if stream.index() == vi {
            rb.video_packets += 1;
            rb.video_start_s = rb.video_start_s.min(pts * vtb);
            video_end = video_end.max((pts + packet.duration() as f64) * vtb);
            vdec.send_packet(&packet).expect("video packet decodes");
            while vdec.receive_frame(&mut vframe).is_ok() {
                rb.video_frames += 1;
            }
        } else if stream.index() == ai {
            rb.audio_start_s = rb.audio_start_s.min(pts * atb);
            adec.send_packet(&packet).expect("audio packet decodes");
            while adec.receive_frame(&mut aframe).is_ok() {
                audio_samples += aframe.samples() as u64;
                rate = aframe.rate();
            }
        }
    }
    vdec.send_eof().unwrap();
    while vdec.receive_frame(&mut vframe).is_ok() {
        rb.video_frames += 1;
    }
    adec.send_eof().unwrap();
    while adec.receive_frame(&mut aframe).is_ok() {
        audio_samples += aframe.samples() as u64;
    }
    rb.video_secs = video_end - rb.video_start_s;
    rb.audio_secs = audio_samples as f64 / rate.max(1) as f64;
    rb
}

#[test]
fn a_clip_across_an_encoder_gap_decodes_whole_and_keeps_audio_in_sync() {
    let _one = nvenc();
    let stats = Arc::new(EncStats::default());
    stats.faults.fast.store(true, Relaxed);
    let ring = Arc::new(TrackRing::for_test("game", 30));
    let mut enc = Loop::start_full(stats.clone(), vec![ring.clone()], true);
    let mut fed = 0u64;
    av_frames(&mut enc, &ring, &mut fed, 1);
    if !enc.nvenc_opened() {
        return;
    }
    av_frames(&mut enc, &ring, &mut fed, 59);
    // The encoder fails; two reopens fail too (about 0.85 s of gap, fast retries).
    stats.faults.open_errors.store(2, Relaxed);
    stats.faults.encode_errors.store(1, Relaxed);
    av_frames(&mut enc, &ring, &mut fed, 30);
    av_frames(&mut enc, &ring, &mut fed, 60);
    assert_eq!(stats.backend.load(Relaxed), 1, "the encoder came back: {:?}", enc.error());
    let path = std::env::temp_dir().join(format!("recall-decode-{}.mp4", uuid::Uuid::new_v4().simple()));
    let secs = enc.save_to(&path);
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    while !path.exists() && std::time::Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(path.exists(), "the clip was never written");
    let rb = read_back(&path);
    let sent_secs = enc.pts() as f64 / 1e6;
    let line = format!("decode check: clip {secs:.3} s, sent {sent_secs:.3} s, {rb:?}");
    // Straight to stderr: the numbers are the evidence.
    let _ = std::io::Write::write_all(&mut std::io::stderr().lock(), format!("DECODE {line}
").as_bytes());
    let _ = std::fs::remove_file(&path);
    // 150 frames sent; the gap (about 0.85 s: retries after 0.1 s, 0.25 s, 0.5 s) drops
    // about 50 of them, NVENC's queue at the error a few more.
    assert!((80..=120).contains(&rb.video_packets), "the clip did not cross a gap of the expected size: {line}");
    assert_eq!(rb.video_frames, rb.video_packets, "every video packet decodes to a frame: {line}");
    assert!(rb.video_secs > sent_secs - 0.15, "the video does not cover the gap: {line}");
    assert!((rb.audio_secs - rb.video_secs).abs() <= SYNC_TOLERANCE_S, "audio and video lengths differ: {line}");
    assert!(rb.video_start_s.abs() <= SYNC_TOLERANCE_S && rb.audio_start_s.abs() <= SYNC_TOLERANCE_S, "streams start apart: {line}");
}
