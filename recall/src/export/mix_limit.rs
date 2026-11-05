//! Level control for a mixed-down track: the sum stays at unity gain (a mix that
//! never peaks is exactly as loud as its sources), and a look-ahead peak limiter
//! holds peaks under the ceiling, so loud sources summed together never hard-clip.
//!
//! The limiter computes the gain each sample needs to stay under the ceiling,
//! holds the lowest of those over the attack time on both sides (so one gain
//! covers a whole crest and the waveform keeps its shape instead of being
//! pressed flat against the ceiling), then smooths it in two linear passes:
//! backwards (the gain ramps down over the attack time *before* a peak, the
//! look-ahead) and forwards (it recovers over the release time after it).
//! Every step only lowers the gain, so no sample exceeds the ceiling.
//! O(samples), no state across calls, deterministic.

/// Output ceiling: -1 dBFS, leaving room for AAC overshoot.
pub(crate) const CEILING: f32 = 0.891_251;
/// Gain ramps reach a peak over 5 ms and recover over 100 ms.
const ATTACK_SECONDS: f32 = 0.005;
const RELEASE_SECONDS: f32 = 0.100;

/// Limits planar `channels` at `rate` in place; samples under the ceiling far from a peak are untouched.
pub(crate) fn limit_mix(channels: &mut [Vec<f32>], rate: u32) {
    let frames = channels.iter().map(Vec::len).max().unwrap_or(0);
    if frames == 0 {
        return;
    }
    let mut gain = vec![1.0f32; frames];
    for channel in channels.iter_mut() {
        for (sample, g) in channel.iter().zip(gain.iter_mut()) {
            let peak = sample.abs();
            if peak > CEILING {
                *g = g.min(CEILING / peak);
            }
        }
    }
    let hold = (ATTACK_SECONDS * rate as f32).max(1.0) as usize;
    let mut gain = window_min(&gain, hold);
    let attack = 1.0 / (ATTACK_SECONDS * rate as f32).max(1.0);
    let release = 1.0 / (RELEASE_SECONDS * rate as f32).max(1.0);
    for n in (0..frames - 1).rev() {
        gain[n] = gain[n].min(gain[n + 1] + attack);
    }
    for n in 1..frames {
        gain[n] = gain[n].min(gain[n - 1] + release);
    }
    for channel in channels.iter_mut() {
        channel.iter_mut().zip(&gain).for_each(|(sample, g)| *sample *= g);
    }
}

/// [`limit_mix`] for interleaved samples: split into planes, limit, interleave again.
pub(crate) fn limit_interleaved(samples: &[f32], channels: u16, rate: u32) -> Vec<f32> {
    let ch = channels.max(1) as usize;
    let mut planes: Vec<Vec<f32>> = (0..ch).map(|c| samples.iter().skip(c).step_by(ch).copied().collect()).collect();
    limit_mix(&mut planes, rate);
    let mut out = vec![0.0f32; samples.len()];
    for (c, plane) in planes.iter().enumerate() {
        out.iter_mut().skip(c).step_by(ch).zip(plane).for_each(|(o, s)| *o = *s);
    }
    out
}

/// Minimum of `values[n - radius ..= n + radius]` for every `n` (monotonic deque, O(n)).
fn window_min(values: &[f32], radius: usize) -> Vec<f32> {
    let mut out = Vec::with_capacity(values.len());
    let mut deque = std::collections::VecDeque::<usize>::new();
    let mut next = 0;
    for n in 0..values.len() {
        while next < values.len() && next <= n + radius {
            while deque.back().is_some_and(|&last| values[last] >= values[next]) {
                deque.pop_back();
            }
            deque.push_back(next);
            next += 1;
        }
        while deque.front().is_some_and(|&first| first + radius < n) {
            deque.pop_front();
        }
        out.push(values[*deque.front().expect("window holds n")]);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tone(hz: f32, amplitude: f32, frames: usize) -> Vec<f32> {
        (0..frames).map(|n| (n as f32 / 48_000.0 * hz * std::f32::consts::TAU).sin() * amplitude).collect()
    }

    #[test]
    fn two_full_scale_tones_stay_under_the_ceiling_without_flat_tops() {
        let frames = 48_000;
        let sum: Vec<f32> = tone(440.0, 1.0, frames).iter().zip(tone(660.0, 1.0, frames)).map(|(a, b)| a + b).collect();
        let mut mix = vec![sum.clone(), sum];
        limit_mix(&mut mix, 48_000);
        let peak = mix[0].iter().fold(0.0f32, |m, s| m.max(s.abs()));
        assert!(peak <= CEILING + 1e-6, "peak {peak}");
        assert!(peak > 0.85, "the limiter holds peaks near the ceiling, not crushed: {peak}");
        assert!(!mix[0].windows(3).any(|w| w.iter().all(|s| s.abs() == peak)), "no flat top");
    }

    #[test]
    fn the_window_minimum_matches_a_plain_scan() {
        let values: Vec<f32> = (0..200).map(|n| ((n * 37 % 101) as f32).sin()).collect();
        for radius in [0, 1, 5, 40] {
            let plain: Vec<f32> = (0..values.len())
                .map(|n| values[n.saturating_sub(radius)..=(n + radius).min(values.len() - 1)].iter().cloned().fold(f32::MAX, f32::min))
                .collect();
            assert_eq!(window_min(&values, radius), plain, "radius {radius}");
        }
    }

    #[test]
    fn a_mix_under_the_ceiling_is_left_exactly_as_it_is() {
        let mut mix = vec![tone(440.0, 0.8, 4_800)];
        let before = mix.clone();
        limit_mix(&mut mix, 48_000);
        assert_eq!(mix, before);
    }
}
