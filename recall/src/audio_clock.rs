//! Where a decoded audio frame belongs on the sample timeline.
//!
//! Container timestamps can disagree with the sample count by a little on every frame:
//! SteelSeries Moments clips (most of the library) stamp their 1024-sample AAC frames 1000
//! or 1100 ticks apart at 48 kHz, so each frame looks 24 samples early or 76 late while the
//! running total never strays more than ~2 ms (measured on Moments clips from 2022-2025).
//! Following those stamps frame by frame cut a 1.6 ms hole or a 0.5 ms overlap into the PCM
//! about 47 times a second: the "choppy" editor audio of 2026-10-27. Samples therefore stay
//! back to back, and a frame is placed at its own timestamp only when it disagrees with the
//! running count by more than [`DRIFT_TOLERANCE_SECONDS`]: a real gap (dropped audio) or overlap.

/// 40 ms: 20 times the worst Moments jitter, and below the A/V offset people notice.
pub(crate) const DRIFT_TOLERANCE_SECONDS: f64 = 0.040;

/// [`DRIFT_TOLERANCE_SECONDS`] in samples at `rate`.
pub(crate) fn drift_tolerance(rate: u32) -> i64 {
    (DRIFT_TOLERANCE_SECONDS * f64::from(rate)).round() as i64
}

/// Start sample for the next frame and whether it was placed by its timestamp (a re-seed).
/// `expected` is where the previous frame ended (`None` before the first frame, which is always
/// placed by its timestamp), `stamped` the frame's timestamp in samples (`None` when absent).
pub(crate) fn frame_start(expected: Option<i64>, stamped: Option<i64>, tolerance: i64) -> (i64, bool) {
    match (expected, stamped) {
        (None, stamped) => (stamped.unwrap_or(0), true),
        (Some(expected), Some(stamped)) if (stamped - expected).abs() > tolerance => (stamped, true),
        (Some(expected), _) => (expected, false),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Moments-style stamps: 1024-sample frames stamped on a 100-tick grid (steps of 1000/1100).
    fn moments_stamp(frame: i64) -> i64 {
        (frame * 1024 + 50) / 100 * 100
    }

    #[test]
    fn jittered_stamps_keep_samples_back_to_back() {
        let tolerance = drift_tolerance(48_000);
        let (mut next, mut reseeds) = (None, 0);
        for frame in 0..5_000 {
            let stamped = moments_stamp(frame);
            assert!(frame == 0 || [1000, 1100].contains(&(stamped - moments_stamp(frame - 1))));
            let (start, reseeded) = frame_start(next, Some(stamped), tolerance);
            reseeds += usize::from(reseeded);
            assert_eq!(start, frame * 1024, "frame {frame} moved off the sample count");
            next = Some(start + 1024);
        }
        assert_eq!(reseeds, 1, "only the first frame is placed by its timestamp");
    }

    #[test]
    fn real_gaps_and_overlaps_follow_the_timestamps() {
        let tolerance = drift_tolerance(48_000);
        assert_eq!(tolerance, 1920);
        // Audio that starts late is placed at its timestamp, even by a few samples.
        assert_eq!(frame_start(None, Some(5), tolerance), (5, true));
        assert_eq!(frame_start(None, None, tolerance), (0, true));
        // 0.5 s of missing audio: silence is kept where it was.
        assert_eq!(frame_start(Some(48_000), Some(72_000), tolerance), (72_000, true));
        // A frame stamped 0.1 s early overlaps: it goes back to its time.
        assert_eq!(frame_start(Some(48_000), Some(43_200), tolerance), (43_200, true));
        // Within the tolerance, or without a stamp, the samples stay contiguous.
        assert_eq!(frame_start(Some(48_000), Some(48_000 + 1920), tolerance), (48_000, false));
        assert_eq!(frame_start(Some(48_000), None, tolerance), (48_000, false));
    }
}
