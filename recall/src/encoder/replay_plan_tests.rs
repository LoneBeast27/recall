//! The planned packet index (restore policy 1(c), approved 2026-11-10), fail-first: a process
//! killed mid-save loses no packet the `.part` holds; the rebuild checks every record against
//! the file (zeros past the end, a torn non-zero tail, audio past the last good video); a plan
//! that stops holding mid-mux hands over to records written through; HEVC's start-code
//! rewrite is predicted exactly; the cost of the plan. Real libx264/libx265 + AAC through the
//! production mux (CPU only), temp folders and app-data root, the fake Recycle Bin.

use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;

use ffmpeg_next as ff;

use super::super::in_progress::{self, Entry};
use super::super::mux::index::PLAN_SKEW;
use super::super::mux::tests::window_job;
use super::super::mux::{produce, OwnedParams, KILL_AFTER};
use super::super::replay_index::{self, claim, idx_of, read, slot, Record};
use super::super::saver::{window_secs, Timing};
use super::replay_review_tests::{moov_at, sandbox, truncate};
use super::*;
use crate::ring::EncodedPacket;

/// `frames` frames of `w`x`h` noise-like libx264 (or `codec`) at 60 fps, keyframe every 30.
/// `buf` with every 4-byte start code made a 3-byte one (still valid Annex B):
/// the muxer's rewrite then changes sizes, which the plan must predict.
fn short_start_codes(buf: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(buf.len());
    let mut i = 0;
    while i < buf.len() {
        if buf[i..].starts_with(&[0, 0, 0, 1]) {
            out.extend_from_slice(&[0, 0, 1]);
            i += 4;
        } else {
            out.push(buf[i]);
            i += 1;
        }
    }
    out
}

fn window_of(codec: &str, frames: usize, w: u32, h: u32) -> Option<(Vec<EncodedPacket>, OwnedParams)> {
    ff::init().unwrap();
    let found = ff::encoder::find_by_name(codec)?;
    let mut ctx = ff::codec::context::Context::new_with_codec(found).encoder().video().ok()?;
    ctx.set_width(w);
    ctx.set_height(h);
    ctx.set_format(ff::format::Pixel::YUV420P);
    ctx.set_time_base(ff::Rational(1, 1_000_000));
    ctx.set_frame_rate(Some(ff::Rational(60, 1)));
    ctx.set_gop(30);
    ctx.set_max_b_frames(0);
    ctx.set_bit_rate(4_000_000);
    ctx.set_flags(ff::codec::Flags::GLOBAL_HEADER);
    let mut opts = ff::Dictionary::new();
    opts.set("preset", "ultrafast");
    opts.set("tune", "zerolatency");
    if codec == "libx265" {
        opts.set("x265-params", "log-level=error:bframes=0");
    } else {
        opts.set("bf", "0");
    }
    let mut enc = ctx.open_with(opts).ok()?;
    let params = OwnedParams::from_encoder(&enc);
    let (mut out, mut pkt) = (Vec::new(), ff::Packet::empty());
    let mut take = |enc: &mut ff::encoder::Video, out: &mut Vec<EncodedPacket>| {
        while enc.receive_packet(&mut pkt).is_ok() {
            let pts = pkt.pts().unwrap_or(0) as u64;
            out.push(EncodedPacket { frame_idx: pts, duration_us: 16_667, keyframe: pkt.is_key(), data: crate::ring::Payload::from(pkt.data().unwrap().to_vec()) });
        }
    };
    let mut seed = 7u32;
    for i in 0..frames {
        let mut vf = ff::frame::Video::new(ff::format::Pixel::YUV420P, w, h);
        for plane in 0..3 {
            for b in vf.data_mut(plane).iter_mut() {
                seed = seed.wrapping_mul(1_103_515_245).wrapping_add(12_345);
                *b = (seed >> 24) as u8;
            }
        }
        vf.set_pts(Some(i as i64 * 16_667));
        enc.send_frame(&vf).unwrap();
        take(&mut enc, &mut out);
    }
    enc.send_eof().unwrap();
    take(&mut enc, &mut out);
    Some((out, params))
}

/// The raw records in an index file (the plan, markers, records written through).
fn raw_records(base: &Path) -> Vec<Vec<u8>> {
    let raw = std::fs::read(idx_of(base)).unwrap();
    let head = 12 + u32::from_le_bytes(raw[8..12].try_into().unwrap()) as usize;
    raw[head..].chunks(32).map(<[u8]>::to_vec).collect()
}

/// A production save of `window` into `<dir>\<name>.mp4.part` (index at this thread's slot),
/// killed after `kill` packets when given.
fn save(dir: &Path, name: &str, window: &(Vec<EncodedPacket>, OwnedParams), kill: Option<u64>) -> PathBuf {
    let part = dir.join(format!("{name}.mp4.part"));
    let job = window_job(&dir.join(format!("{name}.mp4")).to_string_lossy(), window.0.clone(), window.1.clone(), window_secs(&window.0));
    KILL_AFTER.with(|k| k.set(kill));
    let out = produce(&part, &job, &AtomicBool::new(false), &mut Timing::default());
    KILL_AFTER.with(|k| k.set(None));
    assert_eq!(out.is_err(), kill.is_some(), "{:?}", out.err());
    part
}

fn video_in(path: &Path) -> Vec<f64> {
    let Ok(mut input) = ff::format::input(&path) else { return Vec::new() };
    let tb = input.stream(0).map(|s| f64::from(s.time_base())).unwrap_or(0.0);
    input.packets().filter(|(s, _)| s.index() == 0).map(|(_, p)| p.pts().unwrap_or(0) as f64 * tb).collect()
}

/// (stream, pts seconds) of every packet in `path`, in file order.
fn packets_in(path: &Path) -> Vec<(usize, f64)> {
    let Ok(mut input) = ff::format::input(&path) else { return Vec::new() };
    let tbs: Vec<f64> = input.streams().map(|s| f64::from(s.time_base())).collect();
    let mut all: Vec<_> = input.packets().map(|(s, p)| (p.position(), s.index(), p.pts().unwrap_or(0) as f64 * tbs[s.index()])).collect();
    all.sort_by_key(|p| p.0);
    all.into_iter().map(|p| (p.1, p.2)).collect()
}

/// List `part` with its index (claimed out of the slot) and recover it: (clip, logs).
fn recover(dir: &Path, part: &Path) -> (PathBuf, Vec<String>) {
    let index = claim(&slot().unwrap()).unwrap();
    let clip = PathBuf::from(part.to_string_lossy().trim_end_matches(".part"));
    let entry = Entry::replay(part, &clip, Some(&index));
    in_progress::add(entry.clone()).unwrap();
    let (mut clips, mut logs) = (Vec::new(), Vec::new());
    recover_entry(&entry, &mut clips, &mut logs);
    let _ = dir;
    (clip, logs)
}

/// (position, size) of every video sample of a finished file, as FFmpeg reads its `moov`:
/// independent of the packet index under test.
fn video_layout(path: &Path) -> Vec<(u64, u64)> {
    let mut input = ff::format::input(&path).expect("a finished file opens");
    input.packets().filter(|(s, _)| s.index() == 0).map(|(_, p)| (p.position() as u64, p.size() as u64)).collect()
}

/// Video samples of `layout` whole within the first `len` bytes.
fn whole_in(layout: &[(u64, u64)], len: u64) -> usize {
    layout.iter().filter(|(at, size)| at + size <= len).count()
}

/// Video records of a full save of the same window that are whole within the first `len` bytes.
#[allow(dead_code)]
fn whole_video(records: &[Record], len: u64) -> usize {
    records.iter().filter(|r| r.stream == 0 && r.offset + u64::from(r.size) <= len).count()
}

/// The live kill, in a test: the engine dies mid-save (nothing it buffered reaches Windows).
/// Every video packet the `.part` holds whole is in the rebuilt clip.
#[test]
fn a_save_killed_mid_mux_loses_no_packet_the_part_holds() {
    let sb = sandbox("kill");
    let Some(window) = window_of("libx264", 300, 320, 240) else { return eprintln!("libx264 missing; skipped") };
    // The same window saved whole: the same layout, read by FFmpeg from its moov.
    let reference = save(&sb.dir, "R__2026-11-11__15-00-00-000__rr", &window, None);
    let layout = video_layout(&reference);
    let total = read(&slot().unwrap()).unwrap().0.planned as u64;
    let part = save(&sb.dir, "K__2026-11-11__15-00-00-000__kk", &window, Some(total * 9 / 10));
    let len = std::fs::metadata(&part).unwrap().len();
    let expected = whole_in(&layout, len);
    assert!(expected > 200, "the kill came late in the save ({expected} whole video samples)");
    let (clip, logs) = recover(&sb.dir, &part);
    assert_eq!(video_in(&clip).len(), expected, "every whole video packet in the killed .part ({len} B): {logs:?}");
    drop(reference);
}

/// A plain kill at a random byte of a real saved file: the rebuild takes every whole video
/// packet before the cut, and no audio past the last of them.
#[test]
fn a_cut_at_a_random_byte_rebuilds_every_whole_packet_before_it() {
    let sb = sandbox("random-cut");
    let Some(window) = window_of("libx264", 240, 320, 240) else { return eprintln!("libx264 missing; skipped") };
    let part = save(&sb.dir, "S__2026-11-11__15-00-00-000__ss", &window, None);
    let layout = video_layout(&part);
    // The cut lands inside a video sample that has audio frames ahead of it in the file (from
    // about 60% in): those whole audio frames come after the last whole video sample.
    let mut input = ff::format::input(&part).unwrap();
    let mut all: Vec<(usize, u64, u64)> = input.packets().map(|(st, p)| (st.index(), p.position() as u64, p.size() as u64)).collect();
    all.sort_by_key(|p| p.1);
    let from = all.len() * 6 / 10;
    let next_video = (from..all.len() - 2).find(|&i| all[i].0 == 0 && all[i + 1].0 != 0).and_then(|i| (i + 1..all.len()).find(|&j| all[j].0 == 0)).expect("video, audio, video");
    let cut = (all[next_video].1 + all[next_video].2 / 2) as usize;
    drop(input);
    truncate(&part, cut);
    let (clip, logs) = recover(&sb.dir, &part);
    let video = video_in(&clip);
    assert_eq!(video.len(), whole_in(&layout, cut as u64), "{logs:?}");
    // Audio goes in only ahead of a video sample that checked out: none after the last one.
    let last_video = video.iter().cloned().fold(0.0, f64::max);
    let late: Vec<_> = packets_in(&clip).into_iter().filter(|(s, t)| *s != 0 && *t > last_video + 1e-6).collect();
    eprintln!("RANDOM CUT {cut} B: {} video, last at {last_video}, {} audio after it", video.len(), late.len());
    assert!(late.is_empty(), "no audio past the last video sample at {last_video}: {late:?}");
}

/// The plan claims bytes past the real end: zeros (a power cut extended the file). The rebuild
/// stops at the data's end; not one zero packet goes in.
#[test]
fn records_past_the_data_end_are_never_taken() {
    let sb = sandbox("zeros-past-end");
    let Some(window) = window_of("libx264", 180, 320, 240) else { return eprintln!("libx264 missing; skipped") };
    let part = save(&sb.dir, "Z__2026-11-11__15-00-00-000__zz", &window, None);
    let layout = video_layout(&part);
    let cut = moov_at(&part) / 2;
    let mut bytes = std::fs::read(&part).unwrap();
    bytes.truncate(cut);
    bytes.extend(std::iter::repeat_n(0u8, 1 << 20));
    std::fs::write(&part, &bytes).unwrap();
    let (clip, logs) = recover(&sb.dir, &part);
    assert_eq!(video_in(&clip).len(), whole_in(&layout, cut as u64), "{logs:?}");
}

/// A torn tail that is not zeros (garbage where the plan expects packets): the first record
/// that is not a sample ends the rebuild.
#[test]
fn a_torn_non_zero_tail_ends_the_rebuild() {
    let sb = sandbox("torn-tail");
    let Some(window) = window_of("libx264", 180, 320, 240) else { return eprintln!("libx264 missing; skipped") };
    let part = save(&sb.dir, "T__2026-11-11__15-00-00-000__tt", &window, None);
    let layout = video_layout(&part);
    let cut = moov_at(&part) / 2;
    let mut bytes = std::fs::read(&part).unwrap();
    bytes.truncate(moov_at(&part));
    bytes[cut..].iter_mut().enumerate().for_each(|(i, b)| *b = 0xA5 ^ (i as u8));
    std::fs::write(&part, &bytes).unwrap();
    let (clip, logs) = recover(&sb.dir, &part);
    let got = video_in(&clip).len();
    let whole = whole_in(&layout, cut as u64);
    assert!(got <= whole && got + 1 >= whole, "{got} packets of {whole} whole before the garbage: {logs:?}");
}

/// The plan stops holding mid-mux (here forced one byte off at record 40): from there every
/// record is written through after its packet, and the rebuild still takes every packet.
#[test]
fn a_plan_that_stops_holding_hands_over_to_records_written_through() {
    let sb = sandbox("mismatch");
    let Some(window) = window_of("libx264", 120, 320, 240) else { return eprintln!("libx264 missing; skipped") };
    PLAN_SKEW.with(|k| k.set(Some(40)));
    let part = save(&sb.dir, "M__2026-11-11__15-00-00-000__mm", &window, None);
    PLAN_SKEW.with(|k| k.set(None));
    let raw = raw_records(&slot().unwrap());
    let (_, records) = read(&slot().unwrap()).unwrap();
    assert!(raw.iter().any(|c| c.first() == Some(&replay_index::MARKER)), "a marker was written");
    let video = records.iter().filter(|r| r.stream == 0).count();
    truncate(&part, moov_at(&part));
    let (clip, logs) = recover(&sb.dir, &part);
    assert_eq!(video_in(&clip).len(), video, "{logs:?}");
}

/// HEVC: the plan predicts the muxer's start-code rewrite exactly (no marker in a whole save).
#[test]
fn the_plan_predicts_hevc_exactly() {
    let sb = sandbox("hevc");
    let Some((packets, params)) = window_of("libx265", 60, 320, 240) else { return eprintln!("libx265 missing; skipped") };
    let extra = params.extradata();
    assert!(extra.starts_with(&[0, 0, 0, 1]) || extra.starts_with(&[0, 0, 1]), "Annex B extradata: the muxer rewrites the packets");
    let shortened: Vec<EncodedPacket> = packets.iter().map(|p| EncodedPacket { data: crate::ring::Payload::from(short_start_codes(&p.data.to_vec())), ..p.clone() }).collect();
    assert!(shortened.iter().zip(&packets).any(|(a, b)| a.data.len() != b.data.len()), "some 3-byte start codes");
    let part = save(&sb.dir, "H__2026-11-11__15-00-00-000__hh", &(shortened, params), None);
    assert!(video_in(&part).len() == 60, "the HEVC clip plays");
    // Every planned video record is exactly where FFmpeg finds the sample in the finished file.
    let planned: Vec<(u64, u64)> = read(&slot().unwrap()).unwrap().1.iter().filter(|r| r.stream == 0).map(|r| (r.offset, u64::from(r.size))).collect();
    assert_eq!(planned, video_layout(&part), "the plan is the muxer's real layout");
    let raw = raw_records(&slot().unwrap());
    let (head, records) = read(&slot().unwrap()).unwrap();
    assert!(head.planned > 0 && records.len() == head.planned, "planned");
    assert!(!raw.iter().any(|c| c.first() == Some(&replay_index::MARKER)), "the plan held for every packet");
}

/// Check (5), on demand: the plan's cost for a 120 s save, and CPU per frame with and without
/// the index on the saver thread.
#[test]
#[ignore]
fn plan_cost_of_a_120s_save() {
    let sb = sandbox("plan-cost");
    let Some(window) = window_of("libx264", 7200, 64, 64) else { return eprintln!("libx264 missing; skipped") };
    let job = |name: &str| window_job(&sb.dir.join(format!("{name}.mp4")).to_string_lossy(), window.0.clone(), window.1.clone(), window_secs(&window.0));
    let cpu = || crate::thread_qos::thread_cpu_ms();
    // Alternated after a warm-up pair: the median of 4 runs each way counts.
    let mut runs = Vec::new();
    for (k, index) in [false, true].repeat(5).into_iter().enumerate() {
        super::super::mux::NO_INDEX.with(|n| n.set(!index));
        let mut timing = Timing::default();
        let c = cpu();
        produce(&sb.dir.join(format!("{k}.mp4.part")), &job(&k.to_string()), &AtomicBool::new(false), &mut timing).unwrap();
        runs.push((cpu() - c, timing));
    }
    super::super::mux::NO_INDEX.with(|n| n.set(false));
    let median = |odd: usize| { let mut v: Vec<f64> = runs.iter().skip(2).skip(odd).step_by(2).map(|r| r.0).collect(); v.sort_by(f64::total_cmp); (v[1] + v[2]) / 2.0 };
    let (without_cpu, with_cpu, with) = (median(0), median(1), runs[runs.len() - 1].1);
    eprintln!("PLAN COST runs (cpu ms, without/with alternating): {:?}", runs.iter().map(|r| r.0.round()).collect::<Vec<_>>());
    let (_, records) = read(&slot().unwrap()).unwrap();
    eprintln!(
        "PLAN COST 120 s: {} records, index {} B, plan {:.2} ms of index {:.2} ms; saver CPU {:.1} ms with / {:.1} ms without = {:.4} / {:.4} ms per frame",
        records.len(), with.index_bytes, with.index_plan_ms, with.index_ms, with_cpu, without_cpu, with_cpu / 7200.0, without_cpu / 7200.0
    );
}
