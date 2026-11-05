//! TrackRing snapshot/mute behaviour (no WASAPI).

use super::TrackRing;
use std::collections::VecDeque;
use std::sync::Mutex;

fn test_ring(name: &'static str) -> TrackRing {
    TrackRing {
        name,
        source: "mic",
        endpoint: Mutex::new(String::new()),
        rate: 1,
        channels: 1,
        samples: Mutex::new(VecDeque::new()),
        capacity: 16,
    }
}

#[test]
fn microphone_mute_silences_only_future_microphone_samples() {
    let mic = test_ring("Mic");
    let game = test_ring("Game");

    mic.push_capture(&[1.0, 2.0], false);
    game.push_capture(&[3.0, 4.0], false);
    mic.push_capture(&[5.0, 6.0], true);
    game.push_capture(&[7.0, 8.0], true);
    mic.push_capture(&[9.0], false);

    assert_eq!(mic.snapshot(5.0), vec![1.0, 2.0, 0.0, 0.0, 9.0]);
    assert_eq!(game.snapshot(4.0), vec![3.0, 4.0, 7.0, 8.0]);
}

#[test]
fn voice_track_ignores_microphone_mute() {
    let voice = test_ring("Voice");
    voice.push_capture(&[1.0, 2.0], true);
    voice.push_capture(&[3.0], false);
    assert_eq!(voice.snapshot(3.0), vec![1.0, 2.0, 3.0]);
}

#[test]
fn every_track_has_an_editor_title_and_only_voice_is_process_only() {
    use super::{track_title, ProcessSource, TRACKS};
    let rows: Vec<(&str, &str, ProcessSource)> =
        TRACKS.iter().map(|t| (t.name, t.title, t.process)).collect();
    assert_eq!(
        rows,
        vec![
            ("Game", "Game", ProcessSource::GamePid),
            ("Mic", "Microphone", ProcessSource::None),
            ("Voice", "Voice chat", ProcessSource::VoiceApp),
        ]
    );
    assert_eq!(track_title("Mic"), "Microphone");
    assert_eq!(track_title("Voice"), "Voice chat");
    assert_eq!(track_title("Other"), "Other", "unknown names pass through");
    // The native editor labels lanes by these substrings (views/editor.rs).
    for (title, needle) in [("Game", "game"), ("Microphone", "mic"), ("Voice chat", "voice")] {
        assert!(title.to_lowercase().contains(needle));
    }
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
    let ring = TrackRing {
        name: "Game",
        source: "game-process",
        endpoint: Mutex::new(String::new()),
        rate: 10,
        channels: 2,
        samples: Mutex::new(VecDeque::new()),
        capacity: 40,
    };
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
        .map(|name| TrackRing {
            name,
            source: "mic",
            endpoint: Mutex::new(String::new()),
            rate,
            channels,
            samples: Mutex::new(VecDeque::new()),
            capacity,
        })
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
    let audio = TrackRing { rate: RATE, capacity: 200_000, ..test_ring("Game") };
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
