//! Keeps an app track level with the wall clock, so it lines up with the other tracks when a
//! clip is cut from the ring.
//!
//! - No active stream (the app is closed or sends nothing): silence up to the clock. Nothing
//!   live is there to cut, so a block of silence is right.
//! - A stream (re)starts: silence first to bring the track up to the clock, then the sound.
//! - Live sound: the app's device clock can run a little fast or slow. The drift is corrected
//!   one frame at a time, spread through the sound, at most 0.1% of the frames (a duplicated or
//!   dropped frame), and only past `TOLERANCE`. Never a silent block inside live sound.

use std::time::Duration;

/// Drift left alone (arrival jitter is ±10 ms around 20 ms packets).
const TOLERANCE: Duration = Duration::from_millis(20);
/// At most this share of frames is duplicated or dropped.
const MAX_STEP: f64 = 0.001;

pub struct Leveller {
    rate: u32,
    written: u64,
    /// Frames of correction earned (MAX_STEP per frame of sound), capped so a long quiet
    /// stretch never allows a burst.
    credit: f64,
    live: bool,
}

impl Leveller {
    pub fn new(rate: u32) -> Self {
        Self { rate, written: 0, credit: 0.0, live: false }
    }

    fn due(&self, elapsed: Duration) -> u64 {
        (elapsed.as_secs_f64() * self.rate as f64) as u64
    }

    /// No active stream: frames of silence owed up to the clock.
    pub fn idle(&mut self, elapsed: Duration) -> u64 {
        self.live = false;
        let owed = self.due(elapsed).saturating_sub(self.written);
        self.written += owed;
        owed
    }

    /// Live sound (`channels`-interleaved) arriving `elapsed` after the start: what to write.
    pub fn live(&mut self, elapsed: Duration, samples: &[f32], channels: usize) -> Vec<f32> {
        let channels = channels.max(1);
        let frames = samples.len() / channels;
        let due = self.due(elapsed);
        let mut out = Vec::with_capacity(samples.len() + 4 * channels);
        let mut lead = 0u64;
        if !self.live {
            // The stream starts: everything before it was silence.
            self.live = true;
            lead = due.saturating_sub(self.written + frames as u64);
            out.resize(lead as usize * channels, 0.0);
            self.written += lead;
        }
        let lag = due as i64 - (self.written + frames as u64) as i64;
        let tolerance = (TOLERANCE.as_secs_f64() * self.rate as f64) as i64;
        self.credit = (self.credit + frames as f64 * MAX_STEP).min(2.0);
        let steps = if lag.abs() > tolerance { (self.credit.floor() as usize).min(lag.unsigned_abs() as usize).min(frames) } else { 0 };
        self.credit -= steps as f64;
        // The frames to duplicate (behind) or drop (ahead), spread evenly through the packet.
        let at = |k: usize| (2 * k + 1) * frames / (2 * steps.max(1));
        let mut marks = (0..steps).map(at).peekable();
        for frame in 0..frames {
            let sample = &samples[frame * channels..(frame + 1) * channels];
            let marked = marks.peek() == Some(&frame);
            if marked {
                marks.next();
            }
            if marked && lag < 0 {
                continue; // ahead of the clock: drop this frame
            }
            out.extend_from_slice(sample);
            if marked && lag > 0 {
                out.extend_from_slice(sample); // behind: hold this frame one more
            }
        }
        // The lead is counted already; add the sound frames written.
        self.written += (out.len() / channels) as u64 - lead;
        out
    }
}

/// Same exe, any case, with or without `.exe`.
pub fn same_exe(a: &str, b: &str) -> bool {
    let key = |s: &str| {
        let lower = s.trim().to_lowercase();
        lower.strip_suffix(".exe").map_or(lower.clone(), str::to_owned)
    };
    key(a) == key(b)
}
