//! TrackRing snapshot/mute/gain behaviour (no WASAPI).

use super::TrackRing;
use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU32};
use std::sync::Mutex;

fn raw_ring(name: &'static str, input: bool, gain: f32, rate: u32, channels: u16, capacity: usize) -> TrackRing {
    TrackRing {
        name,
        title: Mutex::new(name),
        source: if input { "mic" } else { "desktop-loopback" },
        gain: AtomicU32::new(gain.to_bits()),
        boosted: AtomicBool::new(gain > 1.0),
        input,
        endpoint: Mutex::new(String::new()),
        rate,
        channels,
        samples: Mutex::new(VecDeque::new()),
        capacity,
    }
}

fn test_ring(name: &'static str, input: bool) -> TrackRing {
    raw_ring(name, input, 1.0, 1, 1, 16)
}

#[test]
fn the_mute_hotkey_silences_only_future_input_samples() {
    let mic = test_ring("in-1", true);
    let line = test_ring("in-2", true);
    let game = test_ring("game", false);

    mic.push_capture(&[1.0, 2.0], false);
    game.push_capture(&[3.0, 4.0], false);
    mic.push_capture(&[5.0, 6.0], true);
    line.push_capture(&[5.0], true);
    game.push_capture(&[7.0, 8.0], true);
    mic.push_capture(&[9.0], false);

    assert_eq!(mic.snapshot(5.0), vec![1.0, 2.0, 0.0, 0.0, 9.0]);
    assert_eq!(line.snapshot(1.0), vec![0.0], "every input source is muted");
    assert_eq!(game.snapshot(4.0), vec![3.0, 4.0, 7.0, 8.0]);
}

#[test]
fn an_app_or_output_track_ignores_the_mute_hotkey() {
    let app = test_ring("app-1", false);
    app.push_capture(&[1.0, 2.0], true);
    app.push_capture(&[3.0], false);
    assert_eq!(app.snapshot(3.0), vec![1.0, 2.0, 3.0]);
}

#[test]
fn gain_scales_samples_before_they_are_stored() {
    let quiet = raw_ring("in-1", true, 0.5, 1, 1, 16);
    quiet.push_capture(&[1.0, -0.5], false);
    quiet.push_capture(&[1.0], true);
    assert_eq!(quiet.snapshot(3.0), vec![0.5, -0.25, 0.0]);
}

#[test]
fn pid_values_must_be_positive_integers() {
    use super::parse_pid;
    assert_eq!(parse_pid("K", " 77 "), Ok(77));
    assert_eq!(parse_pid("K", "0").unwrap_err(), "K must be a positive process ID, got 0");
    assert_eq!(
        parse_pid("K", "-3").unwrap_err(),
        "K must be a positive process ID, got \"-3\""
    );
}

/// The pre-memcpy implementation, kept verbatim as the reference.
fn old_snapshot(ring: &TrackRing, secs: f64) -> Vec<f32> {
    let want = (secs * ring.rate as f64) as usize * ring.channels as usize;
    let buf = ring.samples.lock().expect("ring lock");
    let have = buf.len().min(want);
    let mut out = vec![0.0f32; want - have];
    out.extend(buf.iter().skip(buf.len() - have));
    out
}

#[test]
fn slice_snapshot_matches_old_behaviour_across_wraps_and_padding() {
    let ring = raw_ring("Game", false, 1.0, 10, 2, 40);
    let secs_cases = [0.0, 0.05, 0.1, 0.35, 1.0, 1.95, 2.0, 2.5, 7.0];
    // Before any audio: all padding.
    for secs in secs_cases {
        assert_eq!(ring.snapshot(secs), old_snapshot(&ring, secs), "empty, secs={secs}");
    }
    let mut next = 1.0f32;
    let mut saw_split = false;
    for chunk in [3usize, 7, 11, 2, 19, 5, 13, 1, 17, 23, 8, 29] {
        let data: Vec<f32> = (0..chunk)
            .map(|_| {
                next += 1.0;
                next
            })
            .collect();
        ring.push_capture(&data, false);
        saw_split |= !ring.samples.lock().unwrap().as_slices().1.is_empty();
        for secs in secs_cases {
            assert_eq!(
                ring.snapshot(secs),
                old_snapshot(&ring, secs),
                "after chunk {chunk}, secs={secs}"
            );
        }
    }
    assert!(saw_split, "test never exercised a wrapped VecDeque");
    let pcm = ring.snapshot_pcm(1.0);
    assert_eq!((pcm.name, pcm.rate, pcm.channels), ("Game", 10, 2));
    assert_eq!(pcm.samples, old_snapshot(&ring, 1.0));
}

/// Encoder-thread cost of snapshotting two full 120 s 48 kHz stereo rings,
/// and how long the lock `push_capture` waits on is held. Ignored by
/// default: `cargo test -p recall bench_ -- --ignored --nocapture`.
#[test]
#[ignore]
fn bench_full_ring_snapshot() {
    use std::time::Instant;
    let (rate, channels, secs) = (48_000u32, 2u16, 120usize);
    let capacity = secs * rate as usize * channels as usize;
    let rings: Vec<TrackRing> = ["Game", "Mic"]
        .into_iter()
        .map(|name| raw_ring(name, false, 1.0, rate, channels, capacity))
        .collect();
    let chunk: Vec<f32> = (0..960).map(|i| i as f32 * 1e-4).collect();
    for ring in &rings {
        // Overfill so the deque has wrapped, as it has in a live session.
        for _ in 0..(capacity / chunk.len() + 500) {
            ring.push_capture(&chunk, false);
        }
    }
    let started = Instant::now();
    let mut per_track_ms = Vec::new();
    for ring in &rings {
        let t = Instant::now();
        let pcm = ring.snapshot_pcm(119.146);
        per_track_ms.push(t.elapsed().as_secs_f64() * 1000.0);
        assert_eq!(pcm.samples.len(), (119.146 * 48_000.0) as usize * 2);
    }
    let total = started.elapsed().as_secs_f64() * 1000.0;
    // Lock-only share: the same two-slice copy into pre-faulted pages.
    let ring = &rings[0];
    let mut out = vec![0.0f32; capacity];
    for i in (0..out.len()).step_by(1024) {
        out[i] = std::hint::black_box(0.0);
    }
    let t = Instant::now();
    {
        let buf = ring.samples.lock().unwrap();
        let (a, b) = buf.as_slices();
        out[..a.len()].copy_from_slice(a);
        out[a.len()..a.len() + b.len()].copy_from_slice(b);
    }
    let lock_ms = t.elapsed().as_secs_f64() * 1000.0;
    eprintln!(
        "BENCH snapshot: 2 x 119.146 s stereo = {total:.1}ms (per track {per_track_ms:.1?}); \
         lock held for the memcpy ~{lock_ms:.1}ms"
    );
}

#[test]
fn a_configured_name_matches_exactly_before_tokens() {
    let names: Vec<String> = [
        "Microphone (2- USB Audio Device Line)",
        "Microphone (2- USB Audio Device Chat)",
        "Microphone",
    ]
    .map(String::from)
    .to_vec();
    assert_eq!(super::match_endpoint(&names, "Microphone (2- USB Audio Device Chat)"), Some(1));
    assert_eq!(super::match_endpoint(&names, "microphone"), Some(2), "exact beats the first token match");
    assert_eq!(super::match_endpoint(&names, "device chat"), Some(1), "token-AND still works");
    assert_eq!(super::match_endpoint(&names, "Headset (USB)"), None, "missing -> caller uses the default");
    assert_eq!(super::match_endpoint(&names, "  "), None, "empty = Windows default");
}

/// Same table as `recall-native` `services/meters.rs`: the meter must pick what this records.
#[test]
fn endpoint_matching_is_whole_word_and_folds_the_slashed_o() {
    let names: Vec<String> = ["Virtual B (Mixer)", "Virtual A (Mixer)", "Line (Mixér)", "System (MØNITOR)"].map(String::from).to_vec();
    assert_eq!(super::match_endpoint(&names, "virtual a"), Some(1), "'a' is not found inside another word");
    assert_eq!(super::match_endpoint(&names, "System (MØNITOR)"), Some(3), "exact, non-ASCII");
    assert_eq!(super::match_endpoint(&names, "system monitor"), Some(3), "ø typed as o");
    assert_eq!(super::match_endpoint(&names, "SYSTEM (mønitor)"), Some(3), "case-insensitive Ø/ø");
    assert_eq!(super::match_endpoint(&names, "line mixér"), Some(2));
    assert_eq!(super::match_endpoint(&names, "mix"), None, "a part of a word is not a word");
}

/// A save's audio covers exactly its video window on the capture clock, whether
/// the newest frame is held up to the request (a live game) or the clip ends at
/// the last frame (the game closed 30 s before the save: the 11-05 drain).
#[test]
fn a_saves_audio_spans_exactly_its_video_window() {
    use crate::encoder::{audio_tail_us, video_end_us};
    use crate::ring::{EncodedPacket, PacketRing};
    use std::time::Duration;

    const RATE: u32 = 1_000; // one sample per ms; each sample's value is its capture time in ms
    const FRAME_US: u64 = 16_667;
    let audio = TrackRing { rate: RATE, capacity: 200_000, ..test_ring("Game", false) };
    let mut video = PacketRing::new(120_000_000);
    // 60 s of game at 60 fps, keyframe each second; the window then closes.
    let frames = 60 * 60;
    for i in 0..frames {
        video.push(EncodedPacket { frame_idx: i * FRAME_US, duration_us: FRAME_US, keyframe: i % 60 == 0, data: vec![0u8; 4].into() });
    }
    let last_frame_end = frames * FRAME_US;
    // Audio keeps flowing to the save moment: 90 s, the request came at 89.995 s.
    let audio_ms = 90_000u64;
    let samples: Vec<f32> = (0..audio_ms).map(|t| t as f32).collect();
    audio.push_capture(&samples, false);
    let request_end = 89_995_000u64;
    let waited = Duration::from_millis(5);

    for (at_last_frame, want_s) in [(true, 120u64), (true, 30), (false, 120), (false, 30)] {
        let end = video_end_us(request_end, video.end_us(), at_last_frame);
        let window = video.save_window_at(want_s * 1_000_000, end);
        let last = window.last().unwrap();
        let video_us = last.frame_idx + last.duration_us; // rebased: the clip's video length
        let video_start = end - video_us;
        let secs = video_us as f64 / 1_000_000.0;
        let tail = audio_tail_us(request_end, waited, end) as f64 / 1_000_000.0;
        let pcm = audio.snapshot_ending(secs, tail);
        let case = format!("at_last_frame={at_last_frame} want={want_s}s");
        // One sample is 1 ms here; frame times are not whole ms, so allow one sample.
        let near = |a: u64, b: u64| a.abs_diff(b) <= 1;
        assert!(near(pcm.len() as u64, video_us / 1_000), "{case}: audio {} ms vs video {} us", pcm.len(), video_us);
        // Front padding (silence) is where the buffer holds no audio; the first real
        // sample sits at the video's start time on the capture clock.
        let first_real = pcm.iter().position(|&s| s != 0.0).unwrap();
        let first_time_ms = pcm[first_real] as u64 - first_real as u64;
        assert!(near(first_time_ms, video_start / 1_000), "{case}: audio starts {first_time_ms} ms, video {video_start} us");
        let audio_end_ms = *pcm.last().unwrap() as u64 + 1;
        assert!(near(audio_end_ms, end / 1_000), "{case}: audio ends {audio_end_ms} ms, video {end} us");
        if at_last_frame {
            assert_eq!(end, last_frame_end, "{case}: the clip ends at the game's last frame");
        } else {
            assert_eq!(end, request_end, "{case}: a live save holds the newest frame to the request");
        }
    }
}

/// A full-scale stereo tone captured through a +12 dB source: the save's limited copy has no
/// flat tops (no runs of identical clipped samples) and peaks at or below the -1 dBFS ceiling.
#[test]
fn a_boosted_source_is_limited_at_save_without_flat_tops() {
    use crate::export::mix_limit::CEILING;
    let rate = 48_000;
    let ring = raw_ring("app-1", false, 10f32.powf(12.0 / 20.0), rate, 2, rate as usize * 2);
    let tone: Vec<f32> = (0..rate as usize)
        .flat_map(|n| {
            let s = (n as f32 / rate as f32 * 440.0 * std::f32::consts::TAU).sin();
            [s, s]
        })
        .collect();
    ring.push_capture(&tone, false);
    let pcm = ring.snapshot_pcm(1.0);
    assert!(pcm.boosted, "+12 dB marks the track for the limiter");
    let raw_peak = pcm.samples.iter().fold(0.0f32, |m, s| m.max(s.abs()));
    assert!(raw_peak > 3.9, "the ring holds the boosted samples: {raw_peak}");

    let out = pcm.limited();
    assert_eq!(out.samples.len(), pcm.samples.len());
    let peak = out.samples.iter().fold(0.0f32, |m, s| m.max(s.abs()));
    assert!(peak <= CEILING + 1e-6, "peak {peak}");
    assert!(peak > 0.85, "held near the ceiling, not crushed: {peak}");
    let left: Vec<f32> = out.samples.iter().step_by(2).copied().collect();
    assert!(!left.windows(3).any(|w| w[0] == w[1] && w[1] == w[2] && w[0].abs() > 0.5), "no flat top");
    assert_eq!(out.samples[0..2], [out.samples[0], out.samples[0]], "channels stay interleaved");
}

#[test]
fn a_source_at_or_below_unity_is_not_marked_for_the_limiter() {
    for gain in [1.0, 0.5] {
        let ring = raw_ring("in-1", true, gain, 48_000, 2, 16);
        ring.push_capture(&[0.9, -0.9], false);
        assert!(!ring.snapshot_pcm(0.0001).boosted, "gain {gain}");
    }
}

#[test]
fn a_live_gain_change_scales_only_new_samples_and_a_boost_stays_limited() {
    let ring = raw_ring("in-1", true, 1.0, 1, 1, 16);
    ring.push_capture(&[1.0], false);
    ring.set_gain_db(-6.0);
    ring.push_capture(&[1.0], false);
    let s = ring.snapshot(2.0);
    assert_eq!(s[0], 1.0, "history keeps its gain");
    assert!((s[1] - 0.501).abs() < 0.01, "{}", s[1]);
    assert!(!ring.boosted());
    ring.set_gain_db(6.0);
    ring.set_gain_db(0.0);
    assert!(ring.boosted(), "boosted samples may still be in the ring");
    ring.set_gain_db(f32::NAN);
    ring.push_capture(&[0.25], false);
    assert_eq!(ring.snapshot(1.0), vec![0.25], "NaN counts as 0 dB");
}

#[test]
fn a_rename_titles_the_next_save() {
    let ring = raw_ring("mic", true, 1.0, 1, 1, 16);
    ring.set_title("  Voice  ");
    assert_eq!(ring.snapshot_pcm(0.0).title, "Voice");
    ring.set_title("   ");
    assert_eq!(ring.title(), "Voice", "a blank name is ignored");
}

/// The countdown case: a game's process loopback stops sending when the game closes. The
/// track's Leveller fills silence to the clock, so a save made 30 s into the drain, cutting
/// 30 s off the newest sample (`encoder::handoff::audio_tail_us`), gets the sound that ran
/// under the clip's last frames, not sound from 30 s earlier.
#[test]
fn a_quiet_or_closed_source_stays_on_the_clock_for_a_later_save() {
    use std::time::Duration;
    let rate = 100;
    let ring = raw_ring("game", false, 1.0, rate, 1, rate as usize * 120);
    let mut level = super::apps::Leveller::new(rate);
    // 10 s of sound in 20 ms packets, sample value = the second it was played in.
    for packet in 0..500u64 {
        let at = Duration::from_millis((packet + 1) * 20);
        let samples = vec![(packet / 50) as f32 + 1.0; 2];
        ring.push_capture(&level.live(at, &samples, 1), false);
    }
    // The game closes at 10 s; 30 s pass with no packets.
    let frames = level.idle(Duration::from_secs(40));
    ring.push_capture(&vec![0.0; frames as usize], false);
    let clip = ring.snapshot_ending(5.0, 30.0);
    assert_eq!(clip.len(), 500);
    assert_eq!(clip[0], 6.0, "starts 5 s before the game closed");
    assert_eq!(clip[499], 10.0, "ends on the last second of sound");
}
