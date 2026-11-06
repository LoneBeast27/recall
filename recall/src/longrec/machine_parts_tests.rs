//! A long recording in parts: the part rate cap, totals and per-part settings (fake sink).

use std::path::PathBuf;
use std::time::{Duration, Instant};

use super::super::{Event, Recorder};
use super::{Cause, PART_GAP};
use crate::longrec::test_sink::{pkt, temp_dir, tracks, Call, FakeFactory, Log, MS};

/// A recording whose part 1 began at the returned instant (keyframe at capture 0),
/// opened with encoder settings `1`.
fn recording(tag: &str) -> (Recorder<FakeFactory>, Log, PathBuf, Instant) {
    let log = Log::default();
    let mut rec = Recorder::new(FakeFactory { log: log.clone(), ..Default::default() }, tracks());
    let path = temp_dir(tag).join("G__2026-10-22__14-05-09__rec.mp4");
    let t0 = Instant::now();
    rec.set_video(Some(1));
    rec.start(path.clone(), Some((0, t0)), t0);
    assert_eq!(rec.video(0, true, pkt(1), false, t0), vec![Event::Started { path: path.clone() }]);
    rec.video(16 * MS, false, pkt(2), false, t0);
    (rec, log, path, t0)
}

fn ms(n: u64) -> Duration {
    Duration::from_millis(n)
}

/// Merge gate 11-07 #2 (P1): every reopen while waiting pushed the next part 2 s on, so a
/// window resized every second never got one (all video and audio dropped).
#[test]
fn a_flap_that_never_stops_still_gets_a_part_within_ten_seconds_of_the_first_reopen() {
    let (mut rec, log, path, t0) = recording("flap");
    let mut started = Vec::new();
    // From 1 s on the size changes every second, for a minute; keyframes every 500 ms;
    // 10 ms audio chunks all along (the chunk at the keyframe's instant comes first).
    for step in 2..=120u64 {
        let at = t0 + ms(step * 500);
        if step % 2 == 0 {
            rec.encoder_reopened(at, Cause::Size, None);
        }
        for k in 1..=50u64 {
            rec.audio(0, &[0.3; 960], at + ms(10 * k));
            if k != 10 {
                continue;
            }
            for e in rec.video(step * 500 * MS + 100 * MS, true, pkt(step as u8), false, at + ms(100)) {
                if let Event::PartStarted { part, .. } = e {
                    started.push((part, step * 500 + 100));
                }
            }
        }
    }
    // Part 1 began at 0, the first reopen came at 1 s: part 2 by 11 s, no sooner than 10 s.
    assert_eq!(started.first(), Some(&(2, 10_100)), "{started:?}");
    assert!(started.windows(2).all(|w| w[1].1 - w[0].1 >= 10_000 && w[1].1 - w[0].1 <= 10_600), "one part per ~10 s: {started:?}");
    assert!(started.len() >= 5, "{started:?}");
    let part2 = path.with_file_name("G__2026-10-22__14-05-09__rec-2.mp4");
    assert!(part2.is_file() || naming_part(&part2).is_file());
    assert!(matches!(&rec.stop()[..], [Event::Stopped { .. }]));
    flap_parts_keep_their_sound(&log.calls());
}

/// Merge gate 11-07 (P2): each part after the first starts its sound with its first frame
/// (one whole 10 ms chunk, no silence ahead of it) and holds sound for its whole length.
fn flap_parts_keep_their_sound(calls: &[Call]) {
    let parts: Vec<&[Call]> = calls.split(|c| matches!(c, Call::Open(_))).skip(1).collect();
    assert!(parts.len() >= 5, "{} parts", parts.len());
    for (i, part) in parts.iter().enumerate() {
        let audio: Vec<i64> = part.iter().filter_map(|c| match c {
            Call::Audio { track: 0, frames } => Some(*frames as i64),
            Call::Silence { track: 0, frames } => Some(-(*frames as i64)),
            _ => None,
        }).collect();
        let video: Vec<(u64, u64)> = part.iter().filter_map(|c| if let Call::Video { pts, dur, .. } = c { Some((*pts, *dur)) } else { None }).collect();
        let (first, last) = (video.first().unwrap(), video.last().unwrap());
        let video_frames = ((last.0 + last.1 - first.0) * 48 / 1_000) as i64;
        let sound: i64 = audio.iter().map(|f| f.abs()).sum();
        if i > 0 {
            assert_eq!(audio.first(), Some(&480), "part {} starts its sound at its first frame: {audio:?}", i + 1);
        }
        assert!(audio.iter().all(|f| *f > -800), "part {}: no silence over a frame: {audio:?}", i + 1);
        assert!((sound - video_frames).abs() <= 48 * 500, "part {}: {sound} audio frames for {video_frames} of video", i + 1);
    }
}

fn naming_part(p: &std::path::Path) -> PathBuf {
    crate::naming::part_of(p)
}

/// Merge gate 11-07 #2 (P1): the final stop said nothing of the time between parts.
#[test]
fn the_final_stop_counts_the_seconds_missed_between_parts() {
    let (mut rec, _log, _path, t0) = recording("gap");
    rec.video(1_000 * MS, false, pkt(3), false, t0 + ms(1_000));
    // Reopen at 3 s, inside the part gap: part 2 starts on the first keyframe from 10 s.
    rec.encoder_reopened(t0 + ms(3_000), Cause::Size, None);
    rec.video(9_000 * MS, true, pkt(4), false, t0 + ms(9_000));
    assert!(matches!(&rec.video(10_000 * MS, true, pkt(5), false, t0 + ms(10_000))[..], [Event::PartStarted { .. }]));
    rec.video(11_000 * MS, false, pkt(6), false, t0 + ms(11_000));
    let events = rec.stop();
    let [Event::Stopped { parts: 2, gap_seconds, .. }] = events[..] else { panic!("{events:?}") };
    // Part 1 ends at 1 s + its last frame (0.984 s held as the tail): ~1.984 s; part 2 begins at 10 s.
    assert!((gap_seconds - 8.016).abs() < 0.01, "{gap_seconds}");
}

/// Merge gate 11-07 (P2): a GPU -> CPU switch whose headers match made a new file.
#[test]
fn a_reopen_with_matching_headers_goes_on_in_the_same_file() {
    let (mut rec, log, path, t0) = recording("same");
    let events = rec.encoder_reopened(t0 + PART_GAP, Cause::Same, Some(2));
    assert!(events.iter().all(|e| matches!(e, Event::Log(_))), "{events:?}");
    assert!(rec.video(40 * MS, false, pkt(3), false, t0 + PART_GAP).is_empty(), "waits for the new encoder's keyframe");
    assert!(rec.video(50 * MS, true, pkt(4), false, t0 + PART_GAP).is_empty(), "no new part");
    let events = rec.stop();
    assert!(matches!(&events[..], [Event::Stopped { path: p, parts: 1, .. }] if *p == path), "{events:?}");
    let tags: Vec<u8> = log.videos().iter().map(|v| v.3).collect();
    assert_eq!(tags, vec![1, 2, 4], "the P-frame before the new keyframe is dropped");
    assert_eq!(log.calls().iter().filter(|c| matches!(c, Call::Open(_))).count(), 1, "one file");
}

/// Merge gate 11-07 (P2): part 2's audio must start with its first video frame.
#[test]
fn part_two_audio_starts_with_its_first_video_frame() {
    let (mut rec, log, _path, t0) = recording("sync");
    let at = t0 + PART_GAP;
    rec.encoder_reopened(at, Cause::Size, None);
    // Audio keeps arriving while part 2 waits for its keyframe (10 ms chunks by arrival).
    for k in 0..5u64 {
        rec.audio(0, &[0.3; 960], at + ms(10 * k));
    }
    let opened = log.calls().len();
    // Part 2's first keyframe: capture 12.003 s = wall t0 + 12.003 s.
    assert!(matches!(&rec.video(12_003 * MS, true, pkt(5), false, t0 + ms(12_003))[..], [Event::PartStarted { .. }]));
    // A 10 ms chunk arriving at 12.008 s covers 11.998 .. 12.008: 5 ms of it are before the frame.
    rec.audio(0, &[0.3; 960], t0 + ms(12_008));
    rec.audio(0, &[0.3; 960], t0 + ms(12_018));
    let after: Vec<i64> = log.calls()[opened..]
        .iter()
        .filter_map(|c| match c {
            Call::Audio { track: 0, frames } => Some(*frames as i64),
            Call::Silence { track: 0, frames } => Some(-(*frames as i64)),
            _ => None,
        })
        .collect();
    // 48 kHz: 5 ms = 240 frames dropped from the front; one video frame is 800.
    assert_eq!(after.first(), Some(&(480 - 240)), "part 2's audio begins at its first frame: {after:?}");
    assert!(after.iter().all(|f| *f > -800), "no silence over a frame: {after:?}");
}

/// Merge gate 11-07 (P2): the final toast showed only the last part's length.
#[test]
fn the_final_stop_reports_the_total_length_and_the_part_count() {
    let (mut rec, _log, _path, t0) = recording("total");
    for i in 2..=10u64 {
        rec.video(i * 500 * MS, false, pkt(i as u8), false, t0 + ms(i * 500));
    }
    let t = t0 + PART_GAP;
    let events = rec.encoder_reopened(t, Cause::Size, None);
    let Some(Event::Continued { seconds: first, total_seconds, part: 1, .. }) = events.last().cloned() else { panic!("{events:?}") };
    assert_eq!(first, total_seconds, "part 1 is the whole recording so far");
    for i in 0..=6u64 {
        rec.video(20_000 * MS + i * 500 * MS, i == 0, pkt(i as u8), false, t + ms(i * 500));
    }
    let events = rec.stop();
    let [Event::Stopped { seconds, parts, total_seconds, .. }] = events[..] else { panic!("{events:?}") };
    assert_eq!(parts, 2);
    assert!((total_seconds - (first + seconds)).abs() < 1e-9, "{total_seconds} = {first} + {seconds}");
    assert!(total_seconds > 8.0, "{total_seconds}");
}

/// Merge gate 11-07 (P2): a part opened with whatever encoder settings were newest,
/// and every reopen was called a size change.
#[test]
fn each_part_opens_with_the_settings_it_was_given_and_names_the_cause() {
    let (mut rec, log, path, t0) = recording("cause");
    let t = t0 + PART_GAP;
    let events = rec.encoder_reopened(t, Cause::Input, Some(2));
    assert!(matches!(events.last(), Some(Event::Continued { cause: Cause::Input, part: 1, .. })), "{events:?}");
    let part2 = path.with_file_name("G__2026-10-22__14-05-09__rec-2.mp4");
    assert_eq!(rec.video(50 * MS, true, pkt(3), false, t), vec![Event::PartStarted { path: part2, part: 2, cause: Cause::Input }]);
    let params: Vec<u64> = log.calls().into_iter().filter_map(|c| if let Call::Params(g) = c { Some(g) } else { None }).collect();
    assert_eq!(params, vec![1, 2], "each part opened with its own encoder's settings");
    // Stopped between parts: the finished parts are the recording.
    let t = t + PART_GAP;
    rec.encoder_reopened(t, Cause::Encoder, Some(3));
    assert!(matches!(&rec.stop()[..], [Event::Stopped { parts: 2, .. }]));
}

/// Feed `rec` 60 fps frames from capture `from_ms` (a keyframe first) to before `to_ms`,
/// each arriving at its own wall time.
fn frames(rec: &mut Recorder<FakeFactory>, t0: Instant, from_ms: u64, to_ms: u64, tag: u8) {
    let mut at = from_ms * MS;
    while at < to_ms * MS {
        rec.video(at, at == from_ms * MS, pkt(tag), false, t0 + Duration::from_micros(at));
        at += 16_667;
    }
}

/// Merge gate 11-07 #4 (P1): an encode error stops the packets; the recovery's reopen comes
/// up to ~60 s later and real NVENC opens on a keyframe. The picture held still from the
/// old encoder's last packet, and the count from the reopen's own time read ~0 s.
#[test]
fn a_same_header_recovery_counts_from_the_last_packet_to_the_new_keyframe() {
    let (mut rec, _log, _path, t0) = recording("frozen");
    frames(&mut rec, t0, 1_000, 5_000, 3); // the last packet is at ~4.99 s: the encode error
    // No packets while the encoder is down; the 5th reopen opens at 45 s on a keyframe.
    rec.encoder_reopened(t0 + ms(45_000), Cause::Same, Some(2));
    frames(&mut rec, t0, 45_000, 46_000, 4);
    // A static screen afterwards: the VFR encoder sends nothing for 30 s. Not a freeze.
    rec.video(76_000 * MS, false, pkt(5), false, t0 + ms(76_000));
    let events = rec.stop();
    let [Event::Stopped { parts: 1, gap_seconds, frozen_seconds, .. }] = events[..] else { panic!("{events:?}") };
    assert_eq!(gap_seconds, 0.0);
    assert!((frozen_seconds - 40.0).abs() < 0.05, "last packet ~5 s to the keyframe at 45 s: {frozen_seconds}");
}

/// The frozen seconds of every part add up; a reopen with no gap in the packets freezes ~nothing.
#[test]
fn frozen_seconds_carry_across_parts() {
    let (mut rec, _log, _path, t0) = recording("frozen-parts");
    rec.video(1_000 * MS, false, pkt(3), false, t0 + ms(1_000));
    rec.encoder_reopened(t0 + ms(4_000), Cause::Same, Some(2));
    rec.video(4_000 * MS, true, pkt(4), false, t0 + ms(4_000)); // 1 s -> 4 s: 3 s
    rec.encoder_reopened(t0 + PART_GAP, Cause::Size, Some(3));
    assert!(matches!(&rec.video(10_000 * MS, true, pkt(5), false, t0 + PART_GAP)[..], [Event::PartStarted { part: 2, .. }]));
    rec.video(10_016 * MS, false, pkt(6), false, t0 + ms(10_016));
    rec.encoder_reopened(t0 + ms(10_032), Cause::Same, Some(4));
    rec.video(10_032 * MS, true, pkt(7), false, t0 + ms(10_032)); // a clean reopen: 16 ms
    rec.video(10_500 * MS, false, pkt(8), false, t0 + ms(10_500));
    rec.encoder_reopened(t0 + ms(18_000), Cause::Same, Some(5));
    rec.video(18_000 * MS, true, pkt(9), false, t0 + ms(18_000)); // 10.5 s -> 18 s: 7.5 s
    let events = rec.stop();
    let [Event::Stopped { parts: 2, frozen_seconds, .. }] = events[..] else { panic!("{events:?}") };
    assert!((frozen_seconds - 10.516).abs() < 0.01, "3 s in part 1, 0.016 + 7.5 s in part 2: {frozen_seconds}");
}

/// Merge gate 11-07 #3/#4 (P1): audio keeps its wall-clock place through a frozen span (the
/// encoder down from 1 s, back on a keyframe at 20 s), so the sound after it lines up.
#[test]
fn audio_stays_in_sync_across_a_frozen_span() {
    let (mut rec, log, _path, t0) = recording("frozen-sync");
    // 10 ms chunks (480 frames at 48 kHz) by arrival, all through the freeze and after it;
    // video at 60 fps to 1 s, nothing, then a reopen on a keyframe at 20 s and video to 25 s.
    for k in 1..=2_500u64 {
        let at = t0 + ms(10 * k);
        rec.audio(0, &[0.25; 960], at);
        match k {
            100 => frames(&mut rec, t0, 20, 1_000, 3),
            2_000 => {
                rec.encoder_reopened(at, Cause::Same, Some(2));
                frames(&mut rec, t0, 20_000, 25_000, 5);
            }
            _ => {}
        }
        rec.tick(at);
    }
    let events = rec.stop();
    let [Event::Stopped { frozen_seconds, .. }] = events[..] else { panic!("{events:?}") };
    assert!((frozen_seconds - 19.0).abs() < 0.05, "{frozen_seconds}");
    let pcm = log.pcm(0);
    // 25 s of sound by 25 s, none of it shifted: frame n is at n / 48 000 s.
    assert_eq!(pcm.len() / 2, 25 * 48_000, "every frame in its wall-clock place");
    assert!(pcm.iter().all(|s| *s == 0.25), "no silence inserted or sound dropped across the freeze");
    let videos = log.videos();
    let key = videos.iter().find(|v| v.3 == 5).expect("the new keyframe was written");
    assert_eq!(key.0, 20_000 * MS, "the keyframe sits at 20 s in the file, where its sound is");
    let held = videos.iter().rev().find(|v| v.3 == 3).unwrap();
    assert_eq!(held.0 + held.1, 20_000 * MS, "the last old frame is held on screen until it");
}
