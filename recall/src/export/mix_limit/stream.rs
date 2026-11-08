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

use super::{needed_gain, window_min_into, Ramps};

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
    /// What the last call handed on, and the window maths' scratch: reused, so a packet
    /// allocates nothing once they have grown (p9 gate P2-c).
    out: Vec<f32>,
    scratch: Scratch,
}

#[derive(Default)]
struct Scratch {
    needed: Vec<f32>,
    held: Vec<f32>,
    gain: Vec<f32>,
    deque: VecDeque<usize>,
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
            out: Vec::new(),
            scratch: Scratch::default(),
        }
    }

    /// Takes one interleaved chunk; returns the frames whose gain is now final, limited (valid
    /// until the next call).
    pub(crate) fn push(&mut self, samples: &[f32]) -> &[f32] {
        let whole = samples.len() / self.channels * self.channels;
        for frame in samples[..whole].chunks_exact(self.channels) {
            self.needed.push_back(frame.iter().fold(1.0f32, |g, s| g.min(needed_gain(*s))));
        }
        self.pending.extend(&samples[..whole]);
        self.taken += whole / self.channels;
        self.emit(false)
    }

    /// End of the stream: the frames still held back, limited.
    pub(crate) fn finish(&mut self) -> &[f32] {
        self.emit(true)
    }

    /// Nothing held or seen needs limiting and the release ramp is back at full gain: every
    /// frame still held goes out exactly as it came in, whatever follows. The long recording
    /// then writes silence through the sink's fast path (`longrec::machine`).
    pub(crate) fn quiet(&self) -> bool {
        self.last_gain >= 1.0 && self.needed.iter().all(|g| *g >= 1.0)
    }

    /// Hands on the frames still held (as they came in when `quiet`) and starts over, as fresh.
    pub(crate) fn drain(&mut self) -> &[f32] {
        self.emit(true);
        self.needed.clear();
        self.pending.clear();
        (self.gain_base, self.emitted, self.taken, self.last_gain) = (0, 0, 0, f32::MAX);
        &self.out
    }

    fn emit(&mut self, end: bool) -> &[f32] {
        self.out.clear();
        // The hold window is final for frames under `known`; the attack ramp for frames under `ready`.
        let known = if end { self.taken } else { self.taken.saturating_sub(self.ramps.hold) };
        let ready = if end { self.taken } else { known.saturating_sub(self.reach) };
        if ready <= self.emitted {
            return &self.out;
        }
        if self.quiet() {
            // Under the ceiling throughout: every gain is 1, so skip the window maths.
            let count = ready - self.emitted;
            self.out.extend(self.pending.drain(..count * self.channels));
            self.last_gain = 1.0;
            self.emitted = ready;
            let keep_from = self.emitted.saturating_sub(self.ramps.hold).max(self.gain_base);
            self.needed.drain(..keep_from - self.gain_base);
            self.gain_base = keep_from;
            return &self.out;
        }
        let Scratch { needed, held, gain, deque } = &mut self.scratch;
        needed.clear();
        needed.extend(self.needed.iter().copied());
        window_min_into(needed, self.ramps.hold, held, deque);
        gain.clear();
        gain.extend_from_slice(&held[self.emitted - self.gain_base..known - self.gain_base]);
        for n in (0..gain.len().saturating_sub(1)).rev() {
            gain[n] = gain[n].min(gain[n + 1] + self.ramps.attack);
        }
        let count = ready - self.emitted;
        for &g in &gain[..count] {
            let g = g.min(self.last_gain + self.ramps.release);
            self.last_gain = g;
            self.out.extend(self.pending.drain(..self.channels).map(|s| s * g));
        }
        self.emitted = ready;
        // Keep the needed gains the hold window of the next frame still reads.
        let keep_from = self.emitted.saturating_sub(self.ramps.hold).max(self.gain_base);
        self.needed.drain(..keep_from - self.gain_base);
        self.gain_base = keep_from;
        &self.out
    }
}

#[cfg(test)]
#[path = "stream_tests.rs"]
mod tests;
