//! Never lose a clip: a replay save's `.part` at save time and at the next start, on real
//! libx264 MP4 bytes (CPU only; skipped when this FFmpeg build lacks libx264).

use std::cell::RefCell;
use std::sync::atomic::AtomicBool;
use std::time::Duration;

use super::super::mux::tests::{window_job, x264_window};
use super::super::mux::ABORT_AFTER;
use super::super::saver::{window_secs, write_clip_to, write_now, Timing};
use super::super::test_scratch::scratch;
use super::*;
use ffmpeg_next as ff;

/// A finished clip's bytes (`write_now`: real mux, trailer, rename), or None without libx264.
pub(crate) fn finished_clip(dir: &Path) -> Option<Vec<u8>> {
    let (window, params) = x264_window()?;
    let secs = window_secs(&window);
    let path = dir.join("source.mp4");
    write_now(&window_job(&path.to_string_lossy(), window, params, secs)).unwrap();
    let bytes = std::fs::read(&path).unwrap();
    std::fs::remove_file(&path).unwrap();
    Some(bytes)
}

/// The same bytes cut where the trailer's `moov` would start (a kill mid-write).
pub(crate) fn cut_before_moov(bytes: &[u8]) -> Vec<u8> {
    let at = bytes.windows(4).rposition(|w| w == b"moov").expect("a finished mp4 has a moov") - 4;
    bytes[..at].to_vec()
}

fn video_packets(path: &Path) -> usize {
    let mut input = ff::format::input(path).unwrap();
    input.packets().filter(|(s, _)| s.index() == 0).count()
}

#[test]
fn a_part_killed_mid_save_is_recovered_at_the_next_start() {
    // (e) A sign-out / shutdown force-kills the engine mid-save: what it leaves is found at the
    // next start. Killed after the trailer (rename lost): adopted as a playable clip. Killed
    // before it: kept byte for byte, never swept. Killed right after the header: kept too (this
    // is the one-time scan of an older build's parts, which never recycles; restore policy (d)).
    let dir = scratch("recall-part-recover", "kill");
    let Some(whole) = finished_clip(&dir) else { return eprintln!("libx264 missing; skipped") };
    let after_trailer = dir.join("A__2026-11-10__10-00-00-000__aa.mp4.part");
    let mid_write = dir.join("B__2026-11-10__10-01-00-000__bb.mp4.part");
    let after_header = dir.join("C__2026-11-10__10-02-00-000__cc.mp4.part");
    let longrec = dir.join("G__2026-11-10__10-03-00__rec.mp4.part");
    let cut = cut_before_moov(&whole);
    let header = &whole[..whole.windows(4).position(|w| w == b"mdat").unwrap() + 4];
    std::fs::write(&after_trailer, &whole).unwrap();
    std::fs::write(&mid_write, &cut).unwrap();
    std::fs::write(&after_header, header).unwrap();
    std::fs::write(&longrec, b"fragments").unwrap();
    assert_eq!(layout(&mid_write).unwrap(), Layout::Unindexed);
    assert_eq!(layout(&after_header).unwrap(), Layout::NoPackets);

    let fresh = recover_replay_parts(&dir);
    assert!(fresh.is_empty(), "written just now: maybe this engine's own save, left alone: {fresh:?}");
    let found = recover_replay_parts_older(&dir, Duration::ZERO);
    let clip = dir.join("A__2026-11-10__10-00-00-000__aa.mp4");
    assert_eq!(
        found,
        vec![
            Found::Adopted { part: after_trailer.clone(), clip: clip.clone() },
            Found::Kept(mid_write.clone()),
            Found::Empty(after_header.clone()),
        ]
    );
    assert!(video_packets(&clip) > 0, "the adopted clip plays");
    assert_eq!(std::fs::read(&mid_write).unwrap(), cut, "kept byte for byte");
    assert!(after_header.exists(), "the one-time scan never recycles");
    assert!(longrec.exists(), "a long recording's part is longrec's");
    for f in &found {
        eprintln!("{}", f.line());
    }
}

#[test]
fn a_zero_packet_part_is_deleted_with_a_log_line_and_any_other_is_kept() {
    // (c) The abort lands right after the header: the real mux wrote no packet.
    let dir = scratch("recall-part-recover", "zero");
    let Some((window, params)) = x264_window() else { return eprintln!("libx264 missing; skipped") };
    let secs = window_secs(&window);
    let target = dir.join("Z.mp4");
    let job = window_job(&target.to_string_lossy(), window, params, secs);
    let part = naming::part_of(&target);
    let lines = RefCell::new(Vec::<String>::new());
    let log = |l: &str| lines.borrow_mut().push(l.to_string());
    let mut timing = Timing::default();
    ABORT_AFTER.with(|a| a.set(Some(0)));
    let result = write_clip_to(&part, &target, &job, &mut super::super::mux::produce, &AtomicBool::new(false), &mut timing, &log);
    ABORT_AFTER.with(|a| a.set(None));
    assert!(result.is_err());
    assert_eq!(timing.part_packets, 0);
    assert!(!part.exists() && !target.exists(), "nothing of the clip was in it");
    assert!(lines.borrow().iter().any(|l| l.contains("no packet was written") && l.contains("Z.mp4.part")), "{:?}", lines.borrow());

    // With packets in it the same failure keeps the file.
    std::fs::write(&part, b"packets").unwrap();
    after_failed_mux(&part, 3, "disk full", &log);
    assert!(part.exists(), "a .part with packets is never deleted");
    assert!(lines.borrow().last().unwrap().contains("kept for recovery"));
}

#[test]
fn an_aborted_real_mux_finishes_a_playable_partial_clip() {
    // The abort lands after 5 packets: the trailer is written for them and the clip renamed.
    let dir = scratch("recall-part-recover", "partial");
    let Some((window, params)) = x264_window() else { return eprintln!("libx264 missing; skipped") };
    let secs = window_secs(&window);
    let target = dir.join("P.mp4");
    let job = window_job(&target.to_string_lossy(), window.clone(), params, secs);
    let mut timing = Timing::default();
    ABORT_AFTER.with(|a| a.set(Some(5)));
    let muxed = write_clip_to(&naming::part_of(&target), &target, &job, &mut super::super::mux::produce, &AtomicBool::new(false), &mut timing, &|_| {});
    ABORT_AFTER.with(|a| a.set(None));
    let muxed = muxed.unwrap();
    assert_eq!(muxed.partial.as_deref(), Some(super::super::saver::ABORTED));
    assert_eq!(timing.part_packets, 5);
    assert!(muxed.frames > 0 && (muxed.frames as usize) < window.len(), "{} of {}", muxed.frames, window.len());
    assert!((muxed.secs as f64) < secs);
    assert_eq!(video_packets(&target), muxed.frames as usize, "the kept clip plays what was written");
}
