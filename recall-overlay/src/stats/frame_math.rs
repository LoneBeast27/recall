//! FPS and 1% low from the target's present timestamps (QPC ticks).
//!
//! Definitions (the island's numbers):
//! - frame time = the time between two consecutive Present calls of the target
//!   process (PresentMon's `MsBetweenPresents`);
//! - FPS = the number of presents in the last 1 s;
//! - 1% low = 1000 / (99th-percentile frame time in ms) over the last 5 s,
//!   nearest-rank percentile (the frame time that 99% of frames are at or under).
//!
//! "Last" ends at the newest present ETW has delivered, not at the wall clock:
//! a real-time session hands events over in buffers (at least once a second),
//! so the wall-clock tail is always incomplete. A window whose newest present
//! is older than [`STALE_SECS`] means the game stopped presenting: no numbers.

use std::collections::VecDeque;

pub const FPS_WINDOW_SECS: i64 = 1;
pub const LOW_WINDOW_SECS: i64 = 5;
/// Newest present this old (wall clock) = nothing to show. ETW's 1 s flush
/// timer plus slack, so a steady game never flickers to "—".
pub const STALE_SECS: i64 = 3;
/// Fewer frame times than this in the 5 s window: no 1% low (one slow frame
/// would be the whole percentile).
pub const MIN_LOW_SAMPLES: usize = 10;
/// Upper bound on kept timestamps (6 s at 5000 FPS); oldest go first.
const MAX_STAMPS: usize = 30_000;

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct FrameStats {
    pub fps: Option<u32>,
    pub low1: Option<f64>,
}

/// The target's present timestamps, ascending, covering a little more than
/// the 5 s window. Events from different CPUs' ETW buffers can arrive out of
/// order, so inserts keep the order.
#[derive(Debug)]
pub struct FrameWindow {
    ticks_per_sec: i64,
    stamps: VecDeque<i64>,
}

impl FrameWindow {
    pub fn new(ticks_per_sec: i64) -> Self {
        Self { ticks_per_sec: ticks_per_sec.max(1), stamps: VecDeque::new() }
    }

    pub fn clear(&mut self) {
        self.stamps.clear();
    }

    /// Adds one present (duplicates are kept: two presents can share a tick).
    pub fn push(&mut self, at: i64) {
        let index = self.stamps.partition_point(|&t| t <= at);
        self.stamps.insert(index, at);
        let newest = *self.stamps.back().unwrap_or(&at);
        let keep_from = newest.saturating_sub((LOW_WINDOW_SECS + 1) * self.ticks_per_sec);
        while self.stamps.front().is_some_and(|&t| t < keep_from) || self.stamps.len() > MAX_STAMPS {
            self.stamps.pop_front();
        }
    }

    /// FPS and 1% low as of `now` (QPC ticks, the clock of the timestamps).
    pub fn stats(&self, now: i64) -> FrameStats {
        let Some(&newest) = self.stamps.back() else { return FrameStats::default() };
        if now.saturating_sub(newest) > STALE_SECS * self.ticks_per_sec {
            return FrameStats::default();
        }
        let since = |secs: i64| newest.saturating_sub(secs * self.ticks_per_sec);
        let fps_from = since(FPS_WINDOW_SECS);
        let fps = self.stamps.iter().rev().take_while(|&&t| t > fps_from).count() as u32;
        let low_from = since(LOW_WINDOW_SECS);
        let first = self.stamps.partition_point(|&t| t < low_from);
        let in_window: Vec<i64> = self.stamps.range(first..).copied().collect();
        let mut frame_ms = frame_times_ms(&in_window, self.ticks_per_sec);
        FrameStats { fps: Some(fps), low1: low_1_percent(&mut frame_ms) }
    }
}

/// Frame times (ms) between consecutive timestamps.
pub fn frame_times_ms(stamps: &[i64], ticks_per_sec: i64) -> Vec<f64> {
    let per_ms = ticks_per_sec.max(1) as f64 / 1000.0;
    stamps.windows(2).map(|pair| (pair[1] - pair[0]) as f64 / per_ms).collect()
}

/// 1000 / the 99th-percentile frame time (nearest rank); `None` below
/// [`MIN_LOW_SAMPLES`] frame times or for a zero percentile.
pub fn low_1_percent(frame_ms: &mut [f64]) -> Option<f64> {
    if frame_ms.len() < MIN_LOW_SAMPLES {
        return None;
    }
    let rank = (frame_ms.len() * 99).div_ceil(100); // 1-based nearest rank
    let (_, p99, _) = frame_ms.select_nth_unstable_by(rank - 1, f64::total_cmp);
    (*p99 > 0.0).then(|| 1000.0 / *p99)
}

#[cfg(test)]
#[path = "frame_math_tests.rs"]
mod tests;
