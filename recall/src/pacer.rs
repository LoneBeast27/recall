//! CFR frame pacer: converts an irregular render/capture cadence into an exact
//! fixed-rate frame-index sequence. Every index 0..N is emitted exactly once —
//! a slow stretch emits the same content under several consecutive indices
//! (frame duplication), a fast stretch emits nothing between ticks (drop).
//! PTS = index / fps by construction, so the encoded stream is CFR (R1).

use std::ops::Range;
use std::time::{Duration, Instant};

/// Cap on how many indices one `due()` call may emit after a stall, so a
/// multi-second hitch doesn't flood the encoder in a single tick. The pacer
/// re-bases instead: indices are SKIPPED and the timeline jumps forward.
/// (A skipped span is a real gap in wall-clock; re-basing keeps A/V sync sane
/// rather than emitting thousands of duplicate frames.)
const MAX_CATCHUP: u64 = 30;

pub struct Pacer {
    fps: u32,
    t0: Option<Instant>,
    next_idx: u64,
}

impl Pacer {
    pub fn new(fps: u32) -> Self {
        Self {
            fps: fps.max(1),
            t0: None,
            next_idx: 0,
        }
    }

    pub fn fps(&self) -> u32 {
        self.fps
    }

    fn deadline(&self, t0: Instant, idx: u64) -> Instant {
        t0 + Duration::from_nanos(idx * 1_000_000_000 / self.fps as u64)
    }

    /// Frame indices due at `now`. First call arms the clock and emits index 0.
    /// After a stall longer than MAX_CATCHUP frames, emits MAX_CATCHUP dups and
    /// re-bases the clock so the timeline continues from `now`.
    pub fn due(&mut self, now: Instant) -> Range<u64> {
        let t0 = *self.t0.get_or_insert(now);
        let start = self.next_idx;
        let mut stop = start;
        while now >= self.deadline(t0, stop) {
            stop += 1;
            if stop - start >= MAX_CATCHUP {
                // Re-base: pretend the stalled span never existed.
                let skipped = self.deadline(t0, stop);
                let lag = now.saturating_duration_since(skipped);
                self.t0 = Some(t0 + lag);
                break;
            }
        }
        self.next_idx = stop;
        start..stop
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(base: Instant, ms: u64) -> Instant {
        base + Duration::from_millis(ms)
    }

    #[test]
    fn steady_cadence_emits_one_per_tick() {
        let base = Instant::now();
        let mut p = Pacer::new(60);
        assert_eq!(p.due(t(base, 0)), 0..1);
        assert_eq!(p.due(t(base, 17)), 1..2); // 16.67ms deadline passed
        assert_eq!(p.due(t(base, 34)), 2..3);
    }

    #[test]
    fn fast_polls_emit_nothing_between_ticks() {
        let base = Instant::now();
        let mut p = Pacer::new(60);
        assert_eq!(p.due(t(base, 0)), 0..1);
        assert_eq!(p.due(t(base, 5)), 1..1); // not due yet
        assert_eq!(p.due(t(base, 10)), 1..1);
        assert_eq!(p.due(t(base, 17)), 1..2);
    }

    #[test]
    fn slow_stretch_duplicates() {
        let base = Instant::now();
        let mut p = Pacer::new(60);
        assert_eq!(p.due(t(base, 0)), 0..1);
        // 100ms later: deadlines at 16.7, 33.3, 50, 66.7, 83.3, 100 all passed.
        assert_eq!(p.due(t(base, 100)), 1..7);
    }

    #[test]
    fn long_stall_caps_and_rebases() {
        let base = Instant::now();
        let mut p = Pacer::new(60);
        assert_eq!(p.due(t(base, 0)), 0..1);
        let burst = p.due(t(base, 10_000)); // 10s stall
        assert_eq!(burst.end - burst.start, MAX_CATCHUP);
        // After re-base the next poll shortly after emits ~1, not hundreds.
        let next = p.due(t(base, 10_020));
        assert!(next.end - next.start <= 2);
    }
}
