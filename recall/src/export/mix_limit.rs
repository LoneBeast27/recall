//! Level control for a mixed-down track: fixed headroom on the sum, then a
//! look-ahead peak limiter, so loud sources summed together never hard-clip.
//!
//! The limiter computes the gain each sample needs to stay under the ceiling,
//! then smooths it in two linear passes: backwards (the gain ramps down over
//! the attack time *before* a peak, the look-ahead) and forwards (it recovers
//! over the release time after it). Both passes only ever lower the gain, so
//! no sample exceeds the ceiling. O(samples), no state across calls,
//! deterministic.

/// Gain on the summed tracks before limiting: -3 dB.
const HEADROOM: f32 = 0.707_946;
/// Output ceiling: -1 dBFS, leaving room for AAC overshoot.
pub(crate) const CEILING: f32 = 0.891_251;
/// Gain ramps reach a peak over 5 ms and recover over 100 ms.
const ATTACK_SECONDS: f32 = 0.005;
const RELEASE_SECONDS: f32 = 0.100;

/// Applies headroom and the limiter to planar `channels` at `rate` in place.
pub(crate) fn limit_mix(channels: &mut [Vec<f32>], rate: u32) {
    let frames = channels.iter().map(Vec::len).max().unwrap_or(0);
    if frames == 0 {
        return;
    }
    let mut gain = vec![1.0f32; frames];
    for channel in channels.iter_mut() {
        for (sample, g) in channel.iter_mut().zip(gain.iter_mut()) {
            *sample *= HEADROOM;
            let peak = sample.abs();
            if peak > CEILING {
                *g = g.min(CEILING / peak);
            }
        }
    }
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
        assert!(peak > 0.5, "the mix stays loud: peak {peak}");
        let flat = mix[0].windows(3).any(|w| w.iter().all(|s| s.abs() >= 0.999));
        assert!(!flat);
    }

    #[test]
    fn a_quiet_mix_only_gets_the_headroom() {
        let mut mix = vec![tone(440.0, 0.5, 4_800)];
        let before = mix[0].clone();
        limit_mix(&mut mix, 48_000);
        for (a, b) in before.iter().zip(&mix[0]) {
            assert!((a * HEADROOM - b).abs() < 1e-6);
        }
    }
}
