use super::super::{limit_interleaved, CEILING};
use super::StreamLimiter;

/// Interleaved stereo: a full-scale 440 Hz tone at +12 dB, then a quiet stretch.
fn boosted_tone(frames: usize) -> Vec<f32> {
    let gain = 10f32.powf(12.0 / 20.0);
    (0..frames)
        .flat_map(|n| {
            let level = if n < frames * 2 / 3 { gain } else { 0.3 };
            let s = (n as f32 / 48_000.0 * 440.0 * std::f32::consts::TAU).sin() * level;
            [s, -s]
        })
        .collect()
}

fn streamed(input: &[f32], chunk_frames: usize) -> Vec<f32> {
    let mut limiter = StreamLimiter::new(2, 48_000);
    let mut out = Vec::new();
    for chunk in input.chunks(chunk_frames * 2) {
        out.extend(limiter.push(chunk));
    }
    out.extend(limiter.finish());
    out
}

#[test]
fn chunked_streaming_matches_the_one_shot_limiter_sample_for_sample() {
    let input = boosted_tone(48_000);
    let whole = limit_interleaved(&input, 2, 48_000);
    for chunk_frames in [1, 7, 441, 480, 4_800, 100_000] {
        let out = streamed(&input, chunk_frames);
        assert_eq!(out.len(), input.len(), "chunks of {chunk_frames}: same sample count");
        let worst = out.iter().zip(&whole).fold(0.0f32, |m, (a, b)| m.max((a - b).abs()));
        assert!(worst <= 1e-6, "chunks of {chunk_frames}: differs by {worst}");
    }
}

#[test]
fn a_boosted_stream_has_no_flat_tops_and_stays_under_the_ceiling() {
    let out = streamed(&boosted_tone(48_000), 480);
    let peak = out.iter().fold(0.0f32, |m, s| m.max(s.abs()));
    assert!(peak <= CEILING + 1e-6, "peak {peak}");
    assert!(peak > 0.85, "held near the ceiling, not crushed: {peak}");
    let left: Vec<f32> = out.iter().step_by(2).copied().collect();
    assert!(!left.windows(3).any(|w| w[0] == w[1] && w[1] == w[2] && w[0].abs() > 0.5), "no flat top");
}

#[test]
fn nothing_is_held_once_finished_and_short_streams_survive() {
    let mut limiter = StreamLimiter::new(2, 48_000);
    assert!(limiter.push(&[0.5, 0.5, 0.25, 0.25]).is_empty(), "two frames are inside the look-ahead");
    assert_eq!(limiter.finish(), vec![0.5, 0.5, 0.25, 0.25], "under the ceiling: untouched");
    assert!(limiter.finish().is_empty());
}
