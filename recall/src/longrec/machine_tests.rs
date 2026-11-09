//! Long-recording state machine against a recording fake sink (no FFmpeg).

use super::*;
use crate::longrec::test_sink::{pkt, temp_dir, tracks, Call, FakeFactory, Log, MS};

fn recorder(tag: &str) -> (Recorder<FakeFactory>, Log, PathBuf) {
    let log = Log::default();
    let rec = Recorder::new(FakeFactory { log: log.clone(), ..Default::default() }, tracks());
    (rec, log, temp_dir(tag).join("G__2026-10-22__14-05-09__rec.mp4"))
}

fn chunk(frames: usize) -> Vec<f32> {
    vec![0.1; frames * 2]
}

#[test]
fn waits_for_a_keyframe_then_writes_each_packet_one_step_late() {
    let (mut rec, log, path) = recorder("basic");
    let t0 = Instant::now();
    assert!(rec.start(path.clone(), Some((1_000 * MS, t0)), t0).is_empty());
    assert!(rec.is_active());
    assert!(rec.video(1_005 * MS, false, pkt(1), false, t0).is_empty(), "P-frame before a keyframe is dropped");
    assert_eq!(rec.video(1_020 * MS, true, pkt(2), false, t0), vec![Event::Started { path: path.clone() }]);
    rec.video(1_036 * MS, false, pkt(3), false, t0);
    rec.video(1_070 * MS, false, pkt(4), false, t0);
    assert_eq!(log.videos(), vec![(0, 16 * MS, true, 2), (16 * MS, 34 * MS, false, 3)], "VFR durations from the next PTS");
    // The tail packet reuses the previous duration: (1070 + 34 - 1020) ms.
    let events = rec.stop();
    assert_eq!(events, vec![Event::Stopped { path: path.clone(), seconds: 0.084, bytes: 4, marks: 0, parts: 1, total_seconds: 0.084, gap_seconds: 0.0, frozen_seconds: 0.0 }]);
    assert_eq!(log.videos().last(), Some(&(50 * MS, 34 * MS, false, 4)));
    assert!(path.is_file(), "renamed into place");
    assert!(!naming::part_of(&path).exists(), "no .part left");
    assert_eq!(log.calls().iter().filter(|c| **c == Call::Finish).count(), 1);
    assert!(!rec.is_active());
}

#[test]
fn dropped_packets_resync_at_the_next_keyframe_and_regressions_are_skipped() {
    let (mut rec, log, path) = recorder("gap");
    let t0 = Instant::now();
    rec.start(path, Some((0, t0)), t0);
    rec.video(0, true, pkt(1), false, t0);
    rec.video(16 * MS, false, pkt(2), false, t0);
    rec.video(48 * MS, false, pkt(3), true, t0); // queue overflowed before this one
    rec.video(64 * MS, false, pkt(4), false, t0);
    rec.video(1_000 * MS, true, pkt(5), false, t0);
    rec.video(900 * MS, false, pkt(6), false, t0); // regression
    rec.video(1_016 * MS, false, pkt(7), false, t0);
    let tags: Vec<u8> = log.videos().iter().map(|v| v.3).collect();
    assert_eq!(tags, vec![1, 2, 5], "3 and 4 skipped until keyframe 5; 6 never written");
    assert_eq!(log.videos()[1], (16 * MS, 984 * MS, false, 2), "the held packet spans the gap");
    rec.stop();
    assert_eq!(log.videos().last().unwrap().3, 7);
}

#[test]
fn audio_is_placed_by_wall_clock_padded_and_trimmed() {
    let (mut rec, log, path) = recorder("audio");
    let t0 = Instant::now();
    // Request at capture 10 s; the keyframe is captured 40 ms later.
    rec.start(path, Some((10_000 * MS, t0)), t0);
    rec.video(10_040 * MS, true, pkt(1), false, t0 + Duration::from_millis(45));
    let wall_base = t0 + Duration::from_millis(40);
    let at = |ms: u64| wall_base + Duration::from_millis(ms);
    // A chunk ending before the recording starts is dropped entirely.
    rec.audio(0, &chunk(480), wall_base - Duration::from_millis(1));
    // 10 ms chunks ending 15 ms and 25 ms in: continuous (within tolerance).
    rec.audio(0, &chunk(480), at(15));
    rec.audio(0, &chunk(480), at(25));
    // 600 ms of nothing (silent loopback), then a chunk: the gap is padded so
    // the chunk sits at its real-time position (30_000 frames in).
    rec.audio(0, &chunk(480), at(635));
    // A device running ahead of real time: the excess is trimmed at the front.
    rec.audio(1, &chunk(48_000), at(1_000));
    rec.audio(1, &chunk(480), at(1_010));
    rec.audio(1, &chunk(9_600), at(1_020)); // 200 ms claimed, only the last 10 ms are new
    rec.audio(1, &chunk(480), at(900)); // entirely behind what was written
    rec.stop();
    // Where the samples land (the limiter holds ~10 ms until stop, so call sizes say nothing):
    // two chunks from frame 0, 29_040 frames of padding, the late chunk at its real time.
    let frames = |track: usize| log.pcm(track).iter().step_by(2).copied().collect::<Vec<f32>>();
    let game = frames(0);
    assert_eq!(game.len(), 30_480);
    assert!(game[..960].iter().all(|s| *s == 0.1) && game[960..30_000].iter().all(|s| *s == 0.0) && game[30_000..].iter().all(|s| *s == 0.1), "placed by wall clock");
    let mic = frames(1);
    assert_eq!(mic.len(), 48_960, "track 1 ends exactly at 1020 ms of real time: the excess was trimmed, the overlapping chunk wrote nothing");
    assert!(mic.iter().all(|s| *s == 0.1), "no padding inside track 1");
}

#[test]
fn tick_pads_silent_tracks_and_times_out_an_armed_start() {
    let (mut rec, log, path) = recorder("tick");
    let t0 = Instant::now();
    rec.start(path.clone(), None, t0);
    assert!(rec.tick(t0 + Duration::from_secs(5)).is_empty());
    rec.video(0, true, pkt(1), false, t0 + Duration::from_secs(5));
    rec.tick(t0 + Duration::from_secs(6));
    // 1 s in, padded up to 250 ms behind real time on both tracks.
    assert_eq!(log.audio(0), vec![-(36_000)]);
    assert_eq!(log.audio(1), vec![-(36_000)]);
    rec.tick(t0 + Duration::from_millis(6_050)); // only 50 ms more: below tolerance
    assert_eq!(log.audio(0).len(), 1);
    rec.stop();

    let (mut rec, _log, path) = recorder("armtimeout");
    rec.start(path, Some((0, t0)), t0);
    let events = rec.tick(t0 + ARM_TIMEOUT);
    assert!(matches!(&events[..], [Event::Failed { kept: None, .. }]), "{events:?}");
    assert!(!rec.is_active());
}

#[test]
fn stop_or_start_in_the_wrong_phase_is_reported_not_fatal() {
    let (mut rec, _log, path) = recorder("phase");
    let t0 = Instant::now();
    assert!(matches!(&rec.stop()[..], [Event::Log(_)]));
    rec.start(path.clone(), Some((0, t0)), t0);
    assert!(matches!(&rec.start(path.clone(), Some((0, t0)), t0)[..], [Event::Log(_)]), "second start ignored");
    assert!(matches!(&rec.stop()[..], [Event::Failed { kept: None, .. }]), "stopped while armed");
    assert!(!naming::part_of(&path).exists());
}

#[test]
fn a_failing_sink_keeps_the_partial_recording() {
    let (_, log, path) = recorder("fail");
    let mut rec = Recorder::new(FakeFactory { log: log.clone(), fail_video_after: Some(2), ..Default::default() }, tracks());
    let t0 = Instant::now();
    rec.start(path.clone(), Some((0, t0)), t0);
    for i in 0..3u64 {
        assert!(rec.video(i * 16 * MS, i == 0, pkt(i as u8), false, t0).iter().all(|e| matches!(e, Event::Started { .. })));
    }
    let events = rec.video(48 * MS, false, pkt(9), false, t0);
    assert_eq!(events, vec![Event::Failed { reason: "disk full".into(), kept: Some(path.clone()) }]);
    assert!(path.is_file() && !naming::part_of(&path).exists(), "partial renamed into place");
    assert!(log.calls().contains(&Call::Abandon));
    assert!(!rec.is_active());

    let (_, log, path) = recorder("failopen");
    let mut rec = Recorder::new(FakeFactory { log, fail_open: true, ..Default::default() }, tracks());
    rec.start(path.clone(), Some((0, t0)), t0);
    let events = rec.video(0, true, pkt(1), false, t0);
    assert_eq!(events, vec![Event::Failed { reason: "no encoder parameters".into(), kept: None }]);
    assert!(!path.exists() && !naming::part_of(&path).exists());
}

#[test]
fn marks_land_in_the_recording_sidecar_and_a_reopen_continues_in_a_new_file() {
    let (mut rec, log, path) = recorder("marks");
    let t0 = Instant::now();
    rec.start(path.clone(), Some((0, t0)), t0);
    rec.mark(5 * MS); // before the first frame: ignored (armed)
    rec.video(100 * MS, true, pkt(1), false, t0);
    rec.mark(600 * MS);
    rec.video(1_100 * MS, false, pkt(2), false, t0);
    rec.mark(1_100 * MS);
    let later = t0 + PART_GAP; // part 1 is older than the part gap: part 2 may start at once
    let events = rec.encoder_reopened(later, Cause::Size, None);
    assert!(matches!(&events[0], Event::Log(_)));
    assert!(matches!(&events[1], Event::Continued { path: p, marks: 2, .. } if *p == path), "{events:?}");
    assert_eq!(events.len(), 2, "{events:?}");
    let sidecar = crate::bookmarks::sidecar_path(&path);
    assert_eq!(std::fs::read_to_string(sidecar).unwrap(), r#"{"version":1,"marks":[0.5,1.0]}"#);
    assert!(path.is_file() && !naming::part_of(&path).exists(), "part 1 finished and renamed");
    assert!(rec.is_active(), "armed for part 2");
    assert!(rec.encoder_reopened(later, Cause::Size, None).is_empty(), "armed: nothing to end");
    // Part 2 starts on the new encoder's first keyframe, with its own timeline.
    let second = path.with_file_name("G__2026-10-22__14-05-09__rec-2.mp4");
    let settled = later + Duration::from_secs(2); // the second reopen came while waiting
    assert!(rec.video(1_150 * MS, false, pkt(3), false, settled).is_empty(), "waits for a keyframe");
    assert_eq!(rec.video(1_200 * MS, true, pkt(4), false, settled), vec![Event::PartStarted { path: second.clone(), part: 2, cause: Cause::Size }]);
    assert_eq!(log.calls().iter().rfind(|c| matches!(c, Call::Open(_))), Some(&Call::Open(naming::part_of(&second))));
    rec.video(1_216 * MS, false, pkt(5), false, settled);
    let events = rec.stop();
    assert!(matches!(&events[..], [Event::Stopped { path: p, .. }] if *p == second), "{events:?}");
    assert_eq!(log.videos().iter().find(|v| v.3 == 4).map(|v| v.0), Some(0), "part 2 starts at 0");
    assert!(second.is_file() && path.is_file(), "both parts kept");
    assert!(!rec.is_active());
}

#[test]
fn each_reopen_takes_the_next_free_part_and_never_overwrites_a_clip() {
    let (mut rec, _log, path) = recorder("parts");
    let name = |n: &str| path.with_file_name(format!("G__2026-10-22__14-05-09__rec{n}.mp4"));
    std::fs::write(name("-3"), b"keep").unwrap(); // another clip with the part-3 name
    let t0 = Instant::now();
    rec.start(path.clone(), Some((0, t0)), t0);
    rec.video(0, true, pkt(1), false, t0);
    rec.video(16 * MS, false, pkt(2), false, t0);
    let (t1, t2) = (t0 + PART_GAP, t0 + PART_GAP * 2);
    assert!(matches!(rec.encoder_reopened(t1, Cause::Size, None).last(), Some(Event::Continued { .. })));
    assert_eq!(rec.video(33 * MS, true, pkt(3), false, t1), vec![Event::PartStarted { path: name("-2"), part: 2, cause: Cause::Size }]);
    rec.video(50 * MS, false, pkt(4), false, t1);
    let events = rec.encoder_reopened(t2, Cause::Size, None);
    assert!(matches!(events.last(), Some(Event::Continued { path: p, .. }) if *p == name("-2")), "{events:?}");
    assert_eq!(rec.video(66 * MS, true, pkt(5), false, t2), vec![Event::PartStarted { path: name("-4"), part: 3, cause: Cause::Size }]);
    assert!(matches!(&rec.stop()[..], [Event::Stopped { path: p, .. }] if *p == name("-4")));
    assert_eq!(std::fs::read(name("-3")).unwrap(), b"keep", "an existing clip is untouched");
    assert!(path.is_file() && name("-2").is_file() && name("-4").is_file());
}

#[test]
fn a_stop_timeout_or_failure_before_the_next_part_names_the_saved_part() {
    let (mut rec, _log, path) = recorder("between");
    let t0 = Instant::now();
    let reopened = |rec: &mut Recorder<FakeFactory>, path: &PathBuf| {
        rec.start(path.clone(), Some((0, t0)), t0);
        rec.video(0, true, pkt(1), false, t0);
        rec.video(16 * MS, false, pkt(2), false, t0);
        assert!(matches!(rec.encoder_reopened(t0 + PART_GAP, Cause::Size, None).last(), Some(Event::Continued { .. })));
    };
    reopened(&mut rec, &path);
    let events = rec.stop();
    assert!(matches!(&events[..], [Event::Stopped { path: p, .. }] if *p == path), "the user's stop saves part 1: {events:?}");
    assert!(!rec.is_active());
    let path = path.with_file_name("G__2026-10-22__14-06-00__rec.mp4");
    reopened(&mut rec, &path);
    assert!(rec.tick(t0 + PART_GAP + ARM_TIMEOUT / 2).is_empty());
    let events = rec.tick(t0 + PART_GAP + ARM_TIMEOUT);
    assert!(matches!(&events[..], [Event::Failed { kept: Some(p), .. }] if *p == path), "{events:?}");
    let path = path.with_file_name("G__2026-10-22__14-07-00__rec.mp4");
    reopened(&mut rec, &path);
    let events = rec.encoder_failed("NVENC encode failed: device lost");
    assert!(matches!(&events[..], [Event::Failed { kept: Some(p), .. }] if *p == path), "{events:?}");
    assert!(!rec.is_active());
}

#[test]
fn audio_timing_uses_the_instant_sampled_with_the_capture_clock() {
    let (mut rec, log, path) = recorder("lateArm");
    let t0 = Instant::now();
    // The request is sampled at t0 (capture 10 s), but the writer only arms
    // 2 s later (a sleeping HDD): the keyframe 40 ms after the request still
    // maps to t0 + 40 ms, so a chunk ending 15 ms into the file lands at once.
    let armed = t0 + Duration::from_secs(2);
    rec.start(path, Some((10_000 * MS, t0)), armed);
    rec.video(10_040 * MS, true, pkt(1), false, armed);
    rec.audio(0, &chunk(480), t0 + Duration::from_millis(55));
    rec.stop();
    // The chunk is the track's first 480 frames (the limiter held it until stop).
    assert_eq!(log.pcm(0), chunk(480), "no 2 s of silence first: audio does not lead video");
}

#[test]
fn a_failing_finish_closes_the_sink_before_keeping_the_partial() {
    let (_, log, path) = recorder("failfinish");
    let mut rec = Recorder::new(FakeFactory { log: log.clone(), fail_finish: true, ..Default::default() }, tracks());
    let t0 = Instant::now();
    rec.start(path.clone(), Some((0, t0)), t0);
    rec.video(0, true, pkt(1), false, t0);
    rec.video(16 * MS, false, pkt(2), false, t0);
    let events = rec.stop();
    assert_eq!(events, vec![Event::Failed { reason: "write trailer: disk full".into(), kept: Some(path.clone()) }]);
    assert!(path.is_file() && !naming::part_of(&path).exists(), "the open .part was closed, then renamed");
    assert!(log.calls().contains(&Call::Abandon));
}

#[test]
fn a_failed_encoder_ends_the_recording_and_reports_the_saved_file() {
    let (mut rec, log, path) = recorder("encfail");
    let t0 = Instant::now();
    assert!(rec.encoder_failed("x").is_empty(), "idle: nothing to end");
    rec.start(path.clone(), Some((0, t0)), t0);
    rec.video(0, true, pkt(1), false, t0);
    rec.video(16 * MS, false, pkt(2), false, t0);
    let events = rec.encoder_failed("NVENC encode failed: device lost");
    let [Event::Failed { reason, kept }] = &events[..] else { panic!("{events:?}") };
    assert!(reason.contains("device lost") && reason.contains("saved"), "{reason}");
    assert_eq!(kept.as_ref(), Some(&path));
    assert!(path.is_file() && !naming::part_of(&path).exists(), "finished cleanly");
    assert_eq!(log.calls().iter().filter(|c| **c == Call::Finish).count(), 1);
    assert!(!rec.is_active());
    rec.start(path.clone(), Some((0, t0)), t0);
    assert!(matches!(&rec.encoder_failed("x")[..], [Event::Failed { kept: None, .. }]), "armed: fails");
    assert!(!rec.is_active());
}

/// A +12 dB source in a long recording: its track comes out limited (no flat tops, peak at or
/// below -1 dBFS) and equal to the one-shot limiter over the whole input, with every frame in
/// place, including the ~10 ms the limiter held back until stop. A 0 dB track is untouched.
#[test]
fn a_boosted_track_is_limited_with_every_frame_in_place() {
    use crate::export::mix_limit::{limit_interleaved, CEILING};
    let log = Log::default();
    let mut rec = Recorder::new(FakeFactory { log: log.clone(), ..Default::default() }, tracks());
    let path = temp_dir("limit").join("G__2026-11-05__16-50-00__rec.mp4");
    let t0 = Instant::now();
    rec.start(path.clone(), Some((1_000 * MS, t0)), t0);
    rec.video(1_000 * MS, true, pkt(1), false, t0);
    let gain = 10f32.powf(12.0 / 20.0);
    let tone: Vec<f32> = (0..48_000)
        .flat_map(|n| {
            let s = (n as f32 / 48_000.0 * 440.0 * std::f32::consts::TAU).sin() * gain;
            [s, s]
        })
        .collect();
    let quiet: Vec<f32> = tone.iter().map(|s| s / gain * 0.5).collect();
    for (k, (loud, soft)) in tone.chunks(960).zip(quiet.chunks(960)).enumerate() {
        let at = t0 + Duration::from_millis(10 * (k as u64 + 1));
        rec.audio(0, loud, at);
        rec.audio(1, soft, at);
    }
    assert!(log.pcm(0).len() < tone.len(), "the limiter holds the newest frames until stop");
    assert!(matches!(rec.stop()[..], [Event::Stopped { .. }]));

    let out = log.pcm(0);
    assert_eq!(out.len(), tone.len(), "same sample count after the flush at stop");
    let whole = limit_interleaved(&tone, 2, 48_000);
    let worst = out.iter().zip(&whole).fold(0.0f32, |m, (a, b)| m.max((a - b).abs()));
    assert!(worst <= 1e-6, "streamed = one-shot (no shift): differs by {worst}");
    let peak = out.iter().fold(0.0f32, |m, s| m.max(s.abs()));
    assert!(peak <= CEILING + 1e-6 && peak > 0.85, "peak {peak}");
    let left: Vec<f32> = out.iter().step_by(2).copied().collect();
    assert!(!left.windows(3).any(|w| w[0] == w[1] && w[1] == w[2] && w[0].abs() > 0.5), "no flat top");
    assert_eq!(log.pcm(1), quiet, "a 0 dB track is written exactly as captured");
}

/// 2026-11-07 audit A3: a source that started at 0 dB and was boosted live (no engine restart)
/// is still limited in a long recording: a track's limiter starts at its first sample above the
/// ceiling, so a fader crossing 0 dB never needs a restart. A track under the ceiling is written
/// exactly as captured.
#[test]
fn a_source_boosted_after_the_engine_started_is_limited_too() {
    use crate::export::mix_limit::CEILING;
    let log = Log::default();
    let mut rec = Recorder::new(FakeFactory { log: log.clone(), ..Default::default() }, tracks());
    let path = temp_dir("live-boost").join("G__2026-11-07__10-00-00__rec.mp4");
    let t0 = Instant::now();
    rec.start(path.clone(), Some((1_000 * MS, t0)), t0);
    rec.video(1_000 * MS, true, pkt(1), false, t0);
    let gain = 10f32.powf(9.0 / 20.0);
    let tone: Vec<f32> = (0..24_000).flat_map(|n| {
        let s = (n as f32 / 48_000.0 * 440.0 * std::f32::consts::TAU).sin() * gain;
        [s, s]
    }).collect();
    let quiet: Vec<f32> = tone.iter().map(|s| s / gain * 0.5).collect();
    for (k, (loud, soft)) in tone.chunks(960).zip(quiet.chunks(960)).enumerate() {
        let at = t0 + Duration::from_millis(10 * (k as u64 + 1));
        rec.audio(0, loud, at);
        rec.audio(1, soft, at);
    }
    assert!(matches!(rec.stop()[..], [Event::Stopped { .. }]));
    let out = log.pcm(0);
    assert_eq!(out.len(), tone.len(), "every frame in place");
    let peak = out.iter().fold(0.0f32, |m, s| m.max(s.abs()));
    assert!(peak <= CEILING + 1e-6, "a live boost clipped the long recording: peak {peak}");
    assert_eq!(log.pcm(1), quiet, "a track under the ceiling is written as captured");
}

/// 2026-11-07 audit R2-deep-audio-2: a track under the ceiling that goes hot mid-stream (a game
/// transient at 0 dB, a plosive on a boosted mic). The limiter runs from the track's first frame,
/// so it ramps the gain down ahead of the peak instead of stepping it at the chunk where the peak
/// arrives (a click). The burst starts 27 frames into chunk 11, where the chunk boundary sits on
/// the sine's peak, and at a zero crossing, so the input itself has no step.
#[test]
fn a_track_that_goes_hot_mid_stream_has_no_gain_step() {
    let log = Log::default();
    let mut rec = Recorder::new(FakeFactory { log: log.clone(), ..Default::default() }, tracks());
    let path = temp_dir("hot-mid-stream").join("G__2026-11-07__12-00-00__rec.mp4");
    let t0 = Instant::now();
    rec.start(path.clone(), Some((1_000 * MS, t0)), t0);
    rec.video(1_000 * MS, true, pkt(1), false, t0);
    let burst = 10 * 480 + 27;
    let tone: Vec<f32> = (0..16 * 480)
        .flat_map(|n| {
            let amp = if n < burst { 0.85 } else { 1.0 };
            let s = (n as f32 / 48_000.0 * 437.5 * std::f32::consts::TAU).sin() * amp;
            [s, s]
        })
        .collect();
    let silent = vec![0.0f32; tone.len()];
    for (k, (hot, zero)) in tone.chunks(960).zip(silent.chunks(960)).enumerate() {
        let at = t0 + Duration::from_millis(10 * (k as u64 + 1));
        rec.audio(0, hot, at);
        rec.audio(1, zero, at);
    }
    assert!(matches!(rec.stop()[..], [Event::Stopped { .. }]));
    let out: Vec<f32> = log.pcm(0).into_iter().step_by(2).collect();
    let input: Vec<f32> = tone.iter().step_by(2).copied().collect();
    assert_eq!(out.len(), input.len(), "every frame in place");
    let steepest = |x: &[f32]| x.windows(2).fold(0.0f32, |m, w| m.max((w[1] - w[0]).abs()));
    let (got, limit) = (steepest(&out), steepest(&input) + 0.02);
    assert!(got <= limit, "a gain step where the limiter engaged: largest step {got}, input's + 0.02 = {limit}");
    let peak = out.iter().fold(0.0f32, |m, s| m.max(s.abs()));
    assert!(peak <= crate::export::mix_limit::CEILING + 1e-6, "peak {peak}");
}

/// p9 gate P2-c: padding a quiet track with silence allocates no zero block per call (it was
/// 38 KB each); the fake sink logs calls only.
#[test]
fn silence_padding_allocates_no_zero_block() {
    let log = Log::default();
    let mut rec = Recorder::new(FakeFactory { log: log.clone(), no_pcm: true, ..Default::default() }, tracks());
    let path = temp_dir("silence-alloc").join("G__2026-11-07__12-00-00__rec.mp4");
    let t0 = Instant::now();
    rec.start(path, None, t0);
    rec.video(0, true, pkt(1), false, t0 + Duration::from_secs(5));
    rec.tick(t0 + Duration::from_secs(6));
    let (_, bytes) = crate::test_alloc::bytes_in(|| rec.tick(t0 + Duration::from_secs(8)));
    assert_eq!(log.audio(0).len(), 2, "the second tick padded again");
    assert!(bytes < 8_192, "two silence pads allocated {bytes} bytes");
    rec.stop();
}
