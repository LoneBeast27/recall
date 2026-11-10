//! Restore policy, fail-first (`.planning/RESTORE-POLICY.md` §6 R1-R3, U1-U3, check (c)): a
//! replay save's packet index, its rebuild after a cut-off save, the disposal of a save with
//! nothing in it, and the `write_frame` layout against the interleaver's on the same input.
//! Real libx264 + AAC through the production mux (CPU only, skipped without libx264), temp
//! folders, a temp app-data root, the fake Recycle Bin.

use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;

use ffmpeg_next as ff;

use super::super::in_progress;
use super::super::mux::index::INTERLEAVED;
use super::super::mux::tests::window_job;
use super::super::mux::{produce, OwnedParams, ABORT_AFTER};
use super::super::recycle::fake;
use super::super::replay_rebuild;
use super::super::saver::{window_secs, Timing};
use super::super::test_scratch::{scratch, Scratch};
use super::*;
use crate::ring::EncodedPacket;

struct Sandbox {
    _root: crate::user_dirs::test_root::Injected,
    dir: Scratch,
}

fn sandbox(tag: &str) -> Sandbox {
    let dir = scratch("recall-replay-restore", tag);
    let root = crate::user_dirs::inject(&dir.join("app-root"));
    Sandbox { _root: root, dir }
}

/// `frames` 64x64 libx264 frames at 60 fps, keyframe every 30, as a save window from 0.
fn window(frames: usize) -> Option<(Vec<EncodedPacket>, OwnedParams)> {
    ff::init().unwrap();
    let codec = ff::encoder::find_by_name("libx264")?;
    let mut ctx = ff::codec::context::Context::new_with_codec(codec).encoder().video().ok()?;
    ctx.set_width(64);
    ctx.set_height(64);
    ctx.set_format(ff::format::Pixel::YUV420P);
    ctx.set_time_base(ff::Rational(1, 1_000_000));
    ctx.set_frame_rate(Some(ff::Rational(60, 1)));
    ctx.set_gop(30);
    ctx.set_max_b_frames(0);
    ctx.set_bit_rate(600_000);
    ctx.set_flags(ff::codec::Flags::GLOBAL_HEADER);
    let mut opts = ff::Dictionary::new();
    opts.set("preset", "ultrafast");
    opts.set("tune", "zerolatency");
    opts.set("bf", "0");
    let mut enc = ctx.open_with(opts).ok()?;
    let params = OwnedParams::from_encoder(&enc);
    let (mut out, mut pkt) = (Vec::new(), ff::Packet::empty());
    let mut take = |enc: &mut ff::encoder::Video, out: &mut Vec<EncodedPacket>| {
        while enc.receive_packet(&mut pkt).is_ok() {
            let pts = pkt.pts().unwrap_or(0) as u64;
            out.push(EncodedPacket { frame_idx: pts, duration_us: 16_667, keyframe: pkt.is_key(), data: crate::ring::Payload::from(pkt.data().unwrap().to_vec()) });
        }
    };
    for i in 0..frames {
        let mut vf = ff::frame::Video::new(ff::format::Pixel::YUV420P, 64, 64);
        let stride = vf.stride(0);
        for y in 0..64usize {
            for x in 0..64usize {
                vf.data_mut(0)[y * stride + x] = ((x * 3 + y + i * 9) & 0xff) as u8;
            }
        }
        vf.data_mut(1).fill(128);
        vf.data_mut(2).fill(128);
        vf.set_pts(Some(i as i64 * 16_667));
        enc.send_frame(&vf).unwrap();
        take(&mut enc, &mut out);
    }
    enc.send_eof().unwrap();
    take(&mut enc, &mut out);
    Some((out, params))
}

/// One production save (`mux::produce`: AAC, mux, packet index at this thread's slot) into
/// `<dir>\<name>.mp4.part`, not renamed. (part, timing).
fn save(dir: &Path, name: &str, frames: usize) -> Option<(PathBuf, Timing)> {
    let (packets, params) = window(frames)?;
    let secs = window_secs(&packets);
    let part = dir.join(format!("{name}.mp4.part"));
    let job = window_job(&dir.join(format!("{name}.mp4")).to_string_lossy(), packets, params, secs);
    let mut timing = Timing::default();
    produce(&part, &job, &AtomicBool::new(false), &mut timing).expect("save");
    Some((part, timing))
}

/// (stream, position, size, pts seconds) of every packet, in file order.
fn packets_in(path: &Path) -> Vec<(usize, i64, usize, f64)> {
    let mut input = ff::format::input(&path).expect("opens");
    let tbs: Vec<f64> = input.streams().map(|s| f64::from(s.time_base())).collect();
    let mut all: Vec<_> = input.packets().map(|(s, p)| (s.index(), p.position() as i64, p.size(), p.pts().unwrap_or(0) as f64 * tbs[s.index()])).collect();
    all.sort_by_key(|p| p.1);
    all
}

fn mdat_payload(bytes: &[u8]) -> usize {
    bytes.windows(4).position(|w| w == b"mdat").expect("an mdat") + 4
}

/// R1: the index's head is on disk before the first packet goes into the `.part`, and it then
/// records every packet the mux wrote (a save stopped before its first packet, one after 3).
#[test]
fn the_packet_index_is_written_before_the_mux() {
    let sb = sandbox("index-first");
    let Some((packets, params)) = window(60) else { return eprintln!("libx264 missing; skipped") };
    let secs = window_secs(&packets);
    let part = sb.dir.join("A__2026-11-10__10-00-00-000__aa.mp4.part");
    let job = window_job(&sb.dir.join("A__2026-11-10__10-00-00-000__aa.mp4").to_string_lossy(), packets, params, secs);
    let mut cut_after = |n: u64| {
        let mut timing = Timing::default();
        ABORT_AFTER.with(|a| a.set(Some(n)));
        let muxed = produce(&part, &job, &AtomicBool::new(false), &mut timing);
        ABORT_AFTER.with(|a| a.set(None));
        (muxed.map(|m| m.partial.is_some()), timing)
    };
    let (none, _) = cut_after(0);
    assert!(none.is_err(), "stopped before the first packet");
    let (head, records) = read(&slot().unwrap()).expect("the index's head was written before any packet");
    assert_eq!((head.part.as_str(), records.len()), (&*part.to_string_lossy(), 0));
    let (three, timing) = cut_after(3);
    assert_eq!(three, Ok(true), "cut short after 3 packets");
    assert_eq!(read(&slot().unwrap()).unwrap().1.len(), 3, "every packet written is indexed");
    assert!(timing.index_bytes > 0, "its cost is in the save timing");
}

/// R3: with `write_frame` the file holds the packets exactly in index order: each packet's
/// offset is the `mdat` payload start plus the sizes before it.
#[test]
fn the_file_holds_the_packets_in_index_order() {
    let sb = sandbox("index-order");
    let Some((part, _)) = save(&sb.dir, "B__2026-11-10__10-00-00-000__bb", 120) else { return eprintln!("libx264 missing; skipped") };
    let (_, records) = read(&slot().unwrap()).unwrap();
    let start = mdat_payload(&std::fs::read(&part).unwrap()) as u64;
    assert_eq!(records.first().map(|r| r.offset), Some(start), "the first packet starts the mdat payload");
    assert!(records.windows(2).all(|w| w[0].offset + u64::from(w[0].size) == w[1].offset), "packets back to back, in index order");
    let indexed: Vec<(usize, i64, usize)> = records.iter().map(|r| (r.stream as usize, r.offset as i64, r.size as usize)).collect();
    let in_file: Vec<(usize, i64, usize)> = packets_in(&part).iter().map(|p| (p.0, p.1, p.2)).collect();
    // The demuxer may leave out a track's last packets past its edit list; every packet it
    // returns is exactly where the index says.
    let missing: Vec<_> = in_file.iter().filter(|p| !indexed.contains(p)).collect();
    assert!(missing.is_empty() && in_file.len() + 4 >= indexed.len(), "every packet where the index says: {} in the file, {} indexed, not indexed: {missing:?}", in_file.len(), indexed.len());
}

/// The save's bytes cut at `fraction` of the way to its `moov` (a kill mid-write).
fn cut(part: &Path, fraction: f64) -> usize {
    let bytes = std::fs::read(part).unwrap();
    let moov = bytes.windows(4).rposition(|w| w == b"moov").unwrap() - 4;
    let at = (moov as f64 * fraction) as usize;
    std::fs::write(part, &bytes[..at]).unwrap();
    at
}

/// R2: a save cut off mid-write is rebuilt with every packet that is whole in it, and plays.
#[test]
fn a_cut_off_save_is_rebuilt_with_every_whole_packet() {
    let sb = sandbox("rebuild");
    let Some((part, _)) = save(&sb.dir, "C__2026-11-10__10-00-00-000__cc", 180) else { return eprintln!("libx264 missing; skipped") };
    let len = cut(&part, 0.6) as u64;
    let (_, records) = read(&slot().unwrap()).unwrap();
    let whole: Vec<_> = records.iter().take_while(|r| r.offset + u64::from(r.size) <= len).collect();
    let whole_video = whole.iter().filter(|r| r.stream == 0).count();
    let out = sb.dir.join("rebuilt.mp4");
    let rebuilt = replay_rebuild::rebuild(&part, &slot().unwrap(), &out);
    assert!(rebuilt.is_ok(), "rebuilt: {rebuilt:?}");
    let rebuilt = rebuilt.unwrap();
    assert_eq!(rebuilt.packets, whole.len(), "every whole packet, nothing past the cut");
    let played = packets_in(&out);
    assert_eq!(played.iter().filter(|p| p.0 == 0).count(), whole_video, "every whole video packet plays");
    assert!(whole_video > 0 && played.len() + 4 >= whole.len());
}

/// R2 (start-up): a listed save is rebuilt into its clip; the cut `.part` and its index then go
/// to the Recycle Bin and the list forgets it.
#[test]
fn a_listed_cut_off_save_becomes_its_clip() {
    let sb = sandbox("rebuild-listed");
    let name = "D__2026-11-10__10-00-00-000__dd";
    let Some((part, _)) = save(&sb.dir, name, 120) else { return eprintln!("libx264 missing; skipped") };
    cut(&part, 0.7);
    let index = claim(&slot().unwrap()).unwrap();
    let clip = sb.dir.join(format!("{name}.mp4"));
    let entry = in_progress::Entry::replay(&part, &clip, Some(&index));
    in_progress::add(entry.clone()).unwrap();
    let (mut clips, mut logs) = (Vec::new(), Vec::new());
    replay_rebuild::recover_entry(&entry, &mut clips, &mut logs);
    assert_eq!(clips, vec![clip.clone()], "{logs:?}");
    assert!(!packets_in(&clip).is_empty(), "the clip plays");
    let binned = fake::binned_under(&sb.dir);
    assert!(binned.contains(&part) && binned.contains(&idx_of(&index)), "the cut part and its index went to the Recycle Bin: {binned:?}");
    assert!(in_progress::list().is_empty());
}

/// U2: a cut-off save with whole packets in it but no index is never recycled: kept, logged.
#[test]
fn a_cut_off_save_without_its_index_is_kept() {
    let sb = sandbox("no-index");
    let Some((part, _)) = save(&sb.dir, "E__2026-11-10__10-00-00-000__ee", 60) else { return eprintln!("libx264 missing; skipped") };
    cut(&part, 0.7);
    let entry = in_progress::Entry::replay(&part, &sb.dir.join("E__2026-11-10__10-00-00-000__ee.mp4"), None);
    let (mut clips, mut logs) = (Vec::new(), Vec::new());
    replay_rebuild::recover_entry(&entry, &mut clips, &mut logs);
    assert!(part.exists() && clips.is_empty(), "kept as it is");
    assert!(logs.iter().any(|l| l.contains("kept") && l.contains("boxes")), "with a diagnostic line: {logs:?}");
    assert!(fake::binned_under(&sb.dir).is_empty());
}

/// U1 + U3: a failed save with no packet in it holds nothing of the clip: a diagnostic line,
/// then the Recycle Bin; on a volume without one it stays where it is.
#[test]
fn a_save_with_no_packet_goes_to_the_recycle_bin_or_stays() {
    let sb = sandbox("no-packet");
    let header = [0u8, 0, 0, 16, b'f', b't', b'y', b'p', b'i', b's', b'o', b'm', 0, 0, 2, 0];
    let (gone, stays) = (sb.dir.join("F__2026-11-10__10-00-00-000__ff.mp4.part"), sb.dir.join("G__2026-11-10__10-00-00-000__gg.mp4.part"));
    std::fs::write(&gone, header).unwrap();
    std::fs::write(&stays, header).unwrap();
    let lines = std::cell::RefCell::new(Vec::new());
    let log = |l: &str| lines.borrow_mut().push(l.to_string());
    assert!(!super::super::part_recover::after_failed_mux(&gone, 0, "disk full", &log));
    assert!(!gone.exists() && fake::binned_under(&sb.dir) == vec![gone.clone()], "to the Recycle Bin");
    assert!(lines.borrow().iter().any(|l| l.contains("unrecoverable") && l.contains("ftyp 16") && l.contains("no packet was written")), "{:?}", lines.borrow());
    fake::without_bin(|| super::super::part_recover::after_failed_mux(&stays, 0, "disk full", &log));
    assert!(stays.exists(), "no Recycle Bin: left where it is");
    assert!(lines.borrow().iter().any(|l| l.contains("left where it is")), "{:?}", lines.borrow());
}

/// Check (c): `write_frame` against the interleaver on the same input. Box tree, per-stream
/// packet order and timing (A/V sync), interleave distance and size, printed as evidence.
#[test]
fn write_frame_keeps_the_streams_and_their_timing() {
    let sb = sandbox("layout");
    let Some((packets, params)) = window(600) else { return eprintln!("libx264 missing; skipped") };
    let secs = window_secs(&packets);
    let mut run = |name: &str, interleaved: bool| {
        let part = sb.dir.join(format!("{name}.mp4.part"));
        let job = window_job(&sb.dir.join(format!("{name}.mp4")).to_string_lossy(), packets.clone(), params.clone(), secs);
        INTERLEAVED.with(|i| i.set(interleaved));
        produce(&part, &job, &AtomicBool::new(false), &mut Timing::default()).unwrap();
        INTERLEAVED.with(|i| i.set(false));
        part
    };
    let (old, new) = (run("H__2026-11-10__10-00-00-000__old", true), run("H__2026-11-10__10-00-00-000__new", false));
    let describe = |p: &Path| {
        let all = packets_in(p);
        let mut streams: Vec<Vec<f64>> = Vec::new();
        let (mut last_video, mut distance) = (0f64, 0f64);
        for (s, _, _, t) in &all {
            if streams.len() <= *s {
                streams.resize(s + 1, Vec::new());
            }
            streams[*s].push(*t);
            if *s == 0 { last_video = *t } else { distance = distance.max((t - last_video).abs()) }
        }
        let size = std::fs::metadata(p).unwrap().len();
        (crate::encoder::recycle::layout_summary(p), streams, distance, size)
    };
    let ((old_tree, old_streams, old_dist, old_size), (new_tree, new_streams, new_dist, new_size)) = (describe(&old), describe(&new));
    eprintln!("LAYOUT interleaved: {old_tree} size={old_size} B interleave_distance={old_dist:.3}s");
    eprintln!("LAYOUT write_frame: {new_tree} size={new_size} B interleave_distance={new_dist:.3}s");
    for (i, (a, b)) in old_streams.iter().zip(&new_streams).enumerate() {
        eprintln!("LAYOUT stream {i}: {} packets, first {:.6}s / {:.6}s, last {:.6}s / {:.6}s", a.len(), a[0], b[0], a[a.len() - 1], b[b.len() - 1]);
    }
    let identical = std::fs::read(&old).unwrap() == std::fs::read(&new).unwrap();
    eprintln!("LAYOUT byte-identical files: {identical}");
    assert_eq!(old_streams, new_streams, "every stream keeps its packets and their times (A/V sync unchanged)");
    assert!(new_size.abs_diff(old_size) * 100 <= old_size, "size within 1%");
    assert!(new_dist <= old_dist + 0.05, "audio stays as close to its video in the file");
}

/// Check (b), measured on demand (`--ignored`): what the packet index costs a 120 s save at
/// 60 fps with 3 audio tracks (7200 video + ~16.9k AAC packets). The index's cost follows the
/// packet count, not the picture size, so 64x64 frames stand in for a game's.
#[test]
#[ignore]
fn index_cost_of_a_120s_save() {
    let sb = sandbox("index-cost");
    let Some((part, timing)) = save(&sb.dir, "I__2026-11-10__10-00-00-000__ii", 7200) else { return eprintln!("libx264 missing; skipped") };
    let (_, records) = read(&slot().unwrap()).unwrap();
    let per_packet_us = timing.index_ms * 1000.0 / records.len() as f64;
    eprintln!(
        "INDEX COST 120 s save: {} records, {} B (index + header), index {:.2} ms ({per_packet_us:.2} us/packet), mux {:.1} ms, part {} B",
        records.len(),
        timing.index_bytes,
        timing.index_ms,
        timing.mux_ms,
        std::fs::metadata(&part).map(|m| m.len()).unwrap_or(0)
    );
    assert!(records.len() > 20_000);
}
