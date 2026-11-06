//! A long recording in parts: the part rate cap, totals and per-part settings (fake sink).

use std::path::PathBuf;
use std::time::{Duration, Instant};

use super::super::{Event, Recorder};
use super::{Cause, PART_GAP, SETTLE};
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

/// Merge gate 11-07 (P2): a size flap made a new part per reopen.
#[test]
fn a_size_flap_inside_ten_seconds_waits_for_the_gap_and_for_the_size_to_settle() {
    let (mut rec, _log, path, t0) = recording("flap");
    // 1 s into part 1 the window is dragged: part 1 ends, part 2 waits for the 10 s gap.
    assert!(matches!(rec.encoder_reopened(t0 + ms(1_000), Cause::Size, None).last(), Some(Event::Continued { part: 1, .. })));
    assert!(rec.video(2_000 * MS, true, pkt(3), false, t0 + ms(2_000)).is_empty(), "no new part within 10 s of the last one");
    // Still changing at 9.5 s: part 2 waits until the size has held for SETTLE.
    assert!(rec.encoder_reopened(t0 + ms(9_500), Cause::Size, None).is_empty());
    assert!(rec.video(10_500 * MS, true, pkt(4), false, t0 + ms(10_500)).is_empty(), "still settling");
    assert_eq!(ms(9_500) + SETTLE, ms(11_500));
    let part2 = path.with_file_name("G__2026-10-22__14-05-09__rec-2.mp4");
    let events = rec.video(11_500 * MS, true, pkt(5), false, t0 + ms(11_500));
    assert_eq!(events, vec![Event::PartStarted { path: part2, part: 2, cause: Cause::Size }]);
    // A reopen of a part older than the gap goes on at the next keyframe.
    let t = t0 + ms(11_500) + PART_GAP;
    assert!(matches!(rec.encoder_reopened(t, Cause::Size, None).last(), Some(Event::Continued { part: 2, .. })));
    assert!(matches!(&rec.video(21_600 * MS, true, pkt(6), false, t)[..], [Event::PartStarted { part: 3, .. }]));
    assert!(matches!(&rec.stop()[..], [Event::Stopped { parts: 3, .. }]));
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
