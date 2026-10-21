//! TrackRing snapshot/mute behaviour (no WASAPI).

use super::TrackRing;
use std::collections::VecDeque;
use std::sync::Mutex;

fn test_ring(name: &'static str) -> TrackRing {
    TrackRing {
        name,
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
