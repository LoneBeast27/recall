//! [`limit_mix`](super::limit_mix) over a stream of interleaved chunks, for the long recording.
//!
//! The one-shot limiter's gain at frame `n` depends on frames up to
//! `n + hold + attack frames` ahead (the hold window, then the backward attack
//! ramp, which cannot reach further than `1 / attack` frames because the gain
//! never exceeds 1). So this keeps that much look-ahead buffered, emits every
//! frame whose gain is final, and [`finish`](StreamLimiter::finish) emits the
//! rest with the same end-of-signal clipping as the one-shot pass. Output is
//! the input's frames, in order, with nothing added or dropped: only *when*
//! they are handed on lags by the look-ahead (about 10 ms at 48 kHz). The same
//! steps in the same order give the same gains as `limit_mix` over the whole
//! signal.

use std::collections::VecDeque;

use super::{needed_gain, window_min, Ramps};

pub(crate) struct StreamLimiter {
    channels: usize,
    ramps: Ramps,
    /// Frames past `n` the backward attack ramp can reach (with 2 frames of margin).
    reach: usize,
    /// Per-frame needed gain for frames `gain_base..taken`.
    needed: VecDeque<f32>,
    gain_base: usize,
    /// Interleaved samples of frames `emitted..taken`.
    pending: VecDeque<f32>,
    emitted: usize,
    taken: usize,
    /// The forward release ramp's last gain.
    last_gain: f32,
}

impl StreamLimiter {
    pub(crate) fn new(channels: u16, rate: u32) -> Self {
        let ramps = Ramps::at(rate);
        let reach = (1.0 / ramps.attack).ceil() as usize + 2;
        StreamLimiter {
            channels: channels.max(1) as usize,
            ramps,
            reach,
            needed: VecDeque::new(),
            gain_base: 0,
            pending: VecDeque::new(),
            emitted: 0,
            taken: 0,
            last_gain: f32::MAX,
        }
    }

    /// Takes one interleaved chunk; returns the frames whose gain is now final, limited.
    pub(crate) fn push(&mut self, samples: &[f32]) -> Vec<f32> {
        let whole = samples.len() / self.channels * self.channels;
        for frame in samples[..whole].chunks_exact(self.channels) {
            self.needed.push_back(frame.iter().fold(1.0f32, |g, s| g.min(needed_gain(*s))));
        }
        self.pending.extend(&samples[..whole]);
        self.taken += whole / self.channels;
        self.emit(false)
    }

    /// End of the stream: the frames still held back, limited.
    pub(crate) fn finish(&mut self) -> Vec<f32> {
        self.emit(true)
    }

    fn emit(&mut self, end: bool) -> Vec<f32> {
        // The hold window is final for frames under `known`; the attack ramp for frames under `ready`.
        let known = if end { self.taken } else { self.taken.saturating_sub(self.ramps.hold) };
        let ready = if end { self.taken } else { known.saturating_sub(self.reach) };
        if ready <= self.emitted {
            return Vec::new();
        }
        let needed: Vec<f32> = self.needed.iter().copied().collect();
        let held = window_min(&needed, self.ramps.hold);
        let mut gain = held[self.emitted - self.gain_base..known - self.gain_base].to_vec();
        for n in (0..gain.len().saturating_sub(1)).rev() {
            gain[n] = gain[n].min(gain[n + 1] + self.ramps.attack);
        }
        let count = ready - self.emitted;
        let mut out = Vec::with_capacity(count * self.channels);
        for &g in &gain[..count] {
            let g = g.min(self.last_gain + self.ramps.release);
            self.last_gain = g;
            out.extend(self.pending.drain(..self.channels).map(|s| s * g));
        }
        self.emitted = ready;
        // Keep the needed gains the hold window of the next frame still reads.
        let keep_from = self.emitted.saturating_sub(self.ramps.hold).max(self.gain_base);
        self.needed.drain(..keep_from - self.gain_base);
        self.gain_base = keep_from;
        out
    }
}

#[cfg(test)]
#[path = "stream_tests.rs"]
mod tests;
