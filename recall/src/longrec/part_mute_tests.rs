//! F-4 (b): a `.part` recovered after a privacy line the engine never applied keeps its video and
//! its game track, and its device tracks are silence from that moment on. The real muxer and
//! AAC encoder write the part (libx264 stands in for NVENC); a temp dir with a drop guard.

use std::path::{Path, PathBuf};

use ffmpeg_next as ff;

use super::super::fmp4::Fmp4Sink;
use super::super::machine::Sink;
use super::super::TrackInfo;
use super::*;

struct TempDir(PathBuf);

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn temp(tag: &str) -> TempDir {
    let dir = std::env::temp_dir().join(format!("recall-partmute-{tag}-{}", uuid::Uuid::new_v4().simple()));
    std::fs::create_dir_all(&dir).unwrap();
    TempDir(dir)
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

/// 3 s of video with a loud tone on both tracks (Game: not a device; Mic: a device), left as
/// a crash leaves it (no finish).
fn crashed_part(dir: &Path) -> Option<PathBuf> {
    let mut enc = x264()?;
    let params = ff::codec::Parameters::from(&enc);
    let part = dir.join("G__2026-11-10__05-00-00__rec.mp4.part");
    let tracks = vec![TrackInfo { name: "Game", rate: 48_000, channels: 2, device: false }, TrackInfo { name: "Mic", rate: 48_000, channels: 2, device: true }];
    let mut sink = Fmp4Sink::create(&part, &params, &tracks).expect("create");
    let tone: Vec<f32> = (0..1_600).map(|i| ((i / 2) as f32 * 0.05).sin() * 0.5).collect();
    let mut pkt = ff::Packet::empty();
    for i in 0..180u64 {
        let mut vf = ff::frame::Video::new(ff::format::Pixel::YUV420P, 64, 64);
        vf.set_pts(Some((i * 16_667) as i64));
        enc.send_frame(&vf).unwrap();
        while enc.receive_packet(&mut pkt).is_ok() {
            let pts = pkt.pts().unwrap_or(0) as u64;
            sink.video(pts, 16_667, pkt.is_key(), pkt.data().unwrap()).unwrap();
        }
        sink.audio(0, &tone).unwrap();
        sink.audio(1, &tone).unwrap();
    }
    drop(sink); // a kill: no finish
    Some(part)
}

/// Per audio stream: the peak after `from` seconds and before `before` seconds.
fn peaks(path: &Path, from: f64, before: f64) -> Vec<(f32, f32)> {
    let mut ictx = ff::format::input(&path).expect("opens");
    let mut out = Vec::new();
    for stream in ictx.streams().filter(|s| s.parameters().medium() == ff::media::Type::Audio).map(|s| (s.index(), s.time_base(), s.parameters())).collect::<Vec<_>>() {
        let (index, tb, params) = stream;
        let mut dec = ff::codec::context::Context::from_parameters(params).unwrap().decoder().audio().unwrap();
        let (mut after, mut early) = (0f32, 0f32);
        let mut frame = ff::frame::Audio::empty();
        for (s, pkt) in ictx.packets() {
            if s.index() != index {
                continue;
            }
            let t = pkt.pts().unwrap_or(0) as f64 * f64::from(tb);
            dec.send_packet(&pkt).unwrap();
            while dec.receive_frame(&mut frame).is_ok() {
                let peak = (0..frame.planes()).flat_map(|c| frame.plane::<f32>(c).iter().copied()).fold(0f32, |m, x| m.max(x.abs()));
                if t >= from {
                    after = after.max(peak);
                }
                if t < before {
                    early = early.max(peak);
                }
            }
        }
        ictx.seek(0, ..).unwrap();
        out.push((after, early));
    }
    out
}

#[test]
fn a_recovered_part_keeps_video_and_game_and_mutes_the_device_tracks_from_the_moment() {
    ff::init().unwrap();
    let dir = temp("mute");
    let Some(part) = crashed_part(&dir.0) else { return eprintln!("libx264 unavailable; skipped") };
    std::fs::write(sidecar_path(&part), "1000000\n0,1\n").unwrap(); // started at t = 1 000 000 ms
    let recovered = crate::longrec::recover_in_muting(&dir.0, Some(1_000_000 + 1_500)); // muted at 1.5 s
    let path = recovered[0].1.as_ref().expect("recovered").clone();
    assert!(!sidecar_path(&part).exists(), "its sidecar is gone");
    let p = peaks(&path, 1.1, 0.8);
    assert!(p[0].0 > 0.1, "the game track records on after the moment: {p:?}");
    assert!(p[1].1 > 0.1, "the device track before the moment is kept: {p:?}");
    assert!(p[1].0 < 1e-3, "the device track from the moment on is silence: {p:?}");
    let src = include_str!("../longrec.rs").split_whitespace().collect::<Vec<_>>().join(" ");
    assert!(src.contains("if let Err(why) = part_mute::mute_before_recovery(&part, mute_from) {"), "recovery mutes before it renames");
}

#[test]
fn a_part_that_cannot_be_muted_is_not_recovered_and_one_with_no_mute_is() {
    ff::init().unwrap();
    let dir = temp("nomute");
    let Some(part) = crashed_part(&dir.0) else { return eprintln!("libx264 unavailable; skipped") };
    let refused = crate::longrec::recover_in_muting(&dir.0, Some(1));
    assert!(refused[0].1.is_err() && part.exists(), "no sidecar: kept as .part, never recovered with the audio");
    let plain = crate::longrec::recover_in_muting(&dir.0, None);
    assert!(plain[0].1.is_ok(), "no privacy line pending: recovered as before");
}

#[test]
fn a_padded_silent_frame_decodes_as_silence_at_its_new_size() {
    ff::init().unwrap();
    let silent = silent_frame(48_000, 2).expect("a silent frame");
    for size in [silent.len(), silent.len() + 1, silent.len() + 130, 600, 2_000] {
        let padded = super::super::aac_fill::pad_to(&silent, size).expect("fits");
        assert_eq!(padded.len(), size);
    }
    let sc = Sidecar { start_unix_ms: 10_000, device: vec![true] };
    assert_eq!(cutoff_secs(&sc, 12_000), 1.5, "the moment minus the margin");
    assert_eq!(cutoff_secs(&sc, 9_000), 0.0, "never before the start");
}
