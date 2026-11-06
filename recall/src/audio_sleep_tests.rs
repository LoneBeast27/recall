//! Transition row T1 (sleep / resume), rival-fixes lane 6: after the PC sleeps, `Instant`
//! jumps by hours. Expected: the silence a source owes the clock (`Leveller::idle`, written
//! by `apps::push_silence` / `audio::fill_silence`) is at most the ring's capacity, and the
//! ring never holds more than its capacity. Fixed in transitions-fixes/fix/sleep-resume.
//! These tests never allocate the owed silence (on master that is ~4 GB for a 3 h jump).

use super::apps::Leveller;
use super::TrackRing;
use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU32};
use std::sync::Mutex;
use std::time::Duration;

const RATE: u32 = 48_000;

fn ring(capacity: usize) -> TrackRing {
    TrackRing {
        name: "game",
        title: Mutex::new("game"),
        source: "desktop-loopback",
        gain: AtomicU32::new(1.0f32.to_bits()),
        boosted: AtomicBool::new(false),
        input: false,
        endpoint: Mutex::new(String::new()),
        rate: RATE,
        channels: 2,
        samples: Mutex::new(VecDeque::new()),
        capacity,
    }
}

/// T1a: a quiet source after a 3 h sleep owes at most one ring (120 s buffer) of silence.
#[test]
#[ignore = "fixed in transitions-fixes/fix/sleep-resume"]
fn t1a_silence_owed_after_a_three_hour_sleep_is_at_most_one_ring() {
    let mut level = Leveller::new(RATE);
    assert_eq!(level.idle(Duration::from_secs(1)), RATE as u64, "awake: one second owed");
    let owed = level.idle(Duration::from_secs(1 + 3 * 3600));
    let ring_frames = 120 * RATE as u64;
    assert!(owed <= ring_frames, "owed {owed} frames ({} s) for a 120 s ring", owed / RATE as u64);
}

/// T1b: whatever one push brings, the ring keeps only its capacity, newest samples last.
/// (The peak while one oversized push is copied in is not observable here; T1a bounds the
/// push itself.)
#[test]
fn t1b_push_capture_keeps_the_ring_at_its_capacity() {
    let r = ring(8);
    let data: Vec<f32> = (0..20).map(|i| i as f32).collect();
    r.push_capture(&data, false);
    let buf = r.samples.lock().unwrap();
    assert_eq!(buf.len(), 8);
    assert_eq!(buf.back(), Some(&19.0));
}
