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
    let (mut rec, _log, path, t0) = recording("flap");
    let mut started = Vec::new();
    // From 1 s on the size changes every second, for a minute; keyframes every 500 ms.
    for step in 2..=120u64 {
        let at = t0 + ms(step * 500);
        if step % 2 == 0 {
            rec.encoder_reopened(at, Cause::Size, None);
        }
        for e in rec.video(step * 500 * MS + 100 * MS, true, pkt(step as u8), false, at + ms(100)) {
            if let Event::PartStarted { part, .. } = e {
                started.push((part, step * 500 + 100));
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
