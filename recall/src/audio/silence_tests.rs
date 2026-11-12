//! The snapshot pads a quiet track up to now from its mark: the clip is the same as with the
//! old 100 ms fill (every sample written before the save), whatever the owner wrote by then.

use std::time::{Duration, Instant};

use super::super::TrackRing;
use super::QuietMark;

const RATE: u32 = 48_000;

/// 0.5 s of a ramp no sample of which is zero (stereo): where it lands is visible.
fn tone() -> Vec<f32> {
    (0..RATE as usize).map(|i| 0.1 + (i % 1000) as f32 * 1e-4).collect()
}

/// The track as the old fill left it: the tone, then every frame owed up to `now` as samples.
fn filled(tone: &[f32], mark: QuietMark, now: Instant, buffer_secs: u32) -> TrackRing {
    let ring = TrackRing::for_test("filled", buffer_secs);
    ring.push_for_test(tone);
    ring.push_silence_frames(mark.owed(RATE, now), None);
    ring
}

/// The track as it is now: the tone, the owner has written nothing since (it is late).
fn marked(tone: &[f32], mark: QuietMark, buffer_secs: u32) -> TrackRing {
    let ring = TrackRing::for_test("marked", buffer_secs);
    ring.push_for_test(tone);
    *ring.live.quiet.lock().unwrap() = Some(mark);
    ring
}

#[test]
fn a_quiet_stretch_as_long_as_the_buffer_cuts_the_same_clip() {
    crate::stop_capture::reset_for_test();
    let tone = tone();
    let frames = (tone.len() / 2) as u64;
    let buffer = 3;
    for quiet in [Duration::from_millis(300), Duration::from_millis(2_700), Duration::from_secs(3), Duration::from_secs(40)] {
        let now = Instant::now();
        let origin = now - Duration::from_millis(500) - quiet;
        let mark = QuietMark { origin, written: frames };
        let (old, new) = (filled(&tone, mark, now, buffer), marked(&tone, mark, buffer));
        for (secs, tail) in [(3.0, 0.0), (2.0, 0.25), (1.0, 0.0)] {
            let want = old.snapshot_ending_at(secs, tail, now);
            let got = new.snapshot_ending_at(secs, tail, now);
            assert!(want == got, "quiet {quiet:?}, clip {secs} s tail {tail} s: not the old clip");
        }
        if quiet >= Duration::from_secs(3) {
            assert!(new.snapshot_ending_at(3.0, 0.0, now).iter().all(|s| *s == 0.0), "a whole buffer of quiet: only silence");
        } else {
            assert!(new.snapshot_ending_at(3.0, 0.0, now).iter().any(|s| *s != 0.0), "the tone is still in the clip");
        }
    }
}

#[test]
fn a_late_fill_after_the_mark_cuts_the_same_clip_as_before_it() {
    crate::stop_capture::reset_for_test();
    let tone = tone();
    let now = Instant::now();
    let mark = QuietMark { origin: now - Duration::from_millis(1_400), written: (tone.len() / 2) as u64 };
    let ring = marked(&tone, mark, 3);
    let before = ring.snapshot_ending_at(2.0, 0.0, now);
    // The owner catches up (its second was up): the same clip.
    let owed = mark.owed(RATE, now - Duration::from_millis(200));
    ring.push_silence_frames(owed, Some(QuietMark { written: mark.written + owed, ..mark }));
    assert!(ring.snapshot_ending_at(2.0, 0.0, now) == before, "a fill moved the clip's audio");
}

/// P2-4: a privacy stop between the quiet mark and the owner's next fill stores nothing, yet the
/// clip still ends at now: the snapshot pads from the mark the stop left.
#[test]
fn a_privacy_stop_before_the_fill_does_not_move_the_quiet_track() {
    crate::stop_capture::reset_for_test();
    let tone = tone();
    let now = Instant::now();
    let mark = QuietMark { origin: now - Duration::from_millis(1_100), written: (tone.len() / 2) as u64 };
    let ring = marked(&tone, mark, 3);
    let reference = filled(&tone, mark, now, 3).snapshot_ending_at(2.0, 0.0, now);
    crate::stop_capture::request();
    ring.push_silence_frames(mark.owed(RATE, now), Some(QuietMark { written: 0, ..mark }));
    ring.clear_quiet();
    let got = ring.snapshot_ending_at(2.0, 0.0, now);
    crate::stop_capture::reset_for_test();
    assert!(got == reference, "the stop shifted the quiet track");
}

#[test]
fn a_fill_is_one_ring_resize_and_one_long_recording_item() {
    let ring = TrackRing::for_test("bulk", 60);
    ring.push_silence_frames(ring.capacity_frames(), None);
    let started = Instant::now();
    for _ in 0..20 {
        ring.push_silence_frames(RATE as u64 * 30, None); // 30 s each, as a long-stalled owner owes
    }
    assert!(started.elapsed() < Duration::from_millis(400), "bulk fills: {:?}", started.elapsed());
    assert_eq!(ring.samples.lock().unwrap().len(), ring.capacity, "never past capacity");
}
