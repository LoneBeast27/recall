//! A recording that stands still: the encoder says it records, but its encoded-frame count
//! has not moved for [`STALL_AFTER`] while the game window is in front and can be seen.
//! This is logged once per stall and never restarts anything. The encoder encodes only
//! the frames capture delivers (VFR, no CFR repeats), so a paused game, a menu or a
//! loading screen stops the count while the capture is alive. A capture restart costs a
//! new D3D11 device, so the watchdog acts only on positive evidence: the capture thread
//! ended (a handler error, or the WGC item closed: windows-capture posts WM_QUIT on
//! `GraphicsCaptureItem.Closed`), or Desktop Duplication asked (`restart_reason`).
//!
//! Time a minimized or covered game, the lock screen or a display that is off sends no
//! frames does not count, and a monitor capture (the idle desktop) is never watched.

use std::time::{Duration, Instant};

/// No new encoded frame this long, with the game in front, is logged as a stall.
pub(crate) const STALL_AFTER: Duration = Duration::from_secs(10);

#[derive(Debug, Default)]
pub(crate) struct Stall {
    /// The last count seen and since when it has not moved (counting only seen time).
    last: Option<(u64, Instant)>,
    /// This stall was logged (until the count moves or the game is out of sight).
    told: bool,
}

impl Stall {
    /// `frames`: the encoded-frame count while the encoder records (`None` otherwise);
    /// `watched`: a game window in front that can be seen. True = log the stall now (once).
    pub(crate) fn observe(&mut self, frames: Option<u64>, watched: bool, now: Instant) -> bool {
        let (Some(n), true) = (frames, watched) else {
            *self = Self::default();
            return false;
        };
        let since = match self.last {
            Some((seen, at)) if seen == n => at,
            _ => {
                *self = Self { last: Some((n, now)), told: false };
                return false;
            }
        };
        if self.told || now.duration_since(since) < STALL_AFTER {
            return false;
        }
        self.told = true;
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One observation per second `0..=to`: (count, watched) at that second; the restart seconds.
    fn run(to: u64, at: impl Fn(u64) -> (Option<u64>, bool)) -> Vec<u64> {
        let t0 = Instant::now();
        let mut s = Stall::default();
        (0..=to).filter(|&i| { let (f, w) = at(i); s.observe(f, w, t0 + Duration::from_secs(i)) }).collect()
    }

    #[test]
    fn a_count_that_stops_for_10_s_in_front_is_told_once_however_long_it_stays() {
        assert_eq!(run(300, |_| (Some(500), true)), [10]);
        assert_eq!(run(35, |i| (Some(if i < 30 { 500 } else { 501 }), true)), [10], "moving again ends it");
        assert_eq!(run(60, |i| (Some(if i < 20 { 500 } else { 501 }), true)), [10, 30], "a new stall is told again");
    }

    #[test]
    fn a_moving_count_never_stalls() {
        assert_eq!(run(120, |i| (Some(i * 60), true)), Vec::<u64>::new());
    }

    #[test]
    fn time_away_or_not_recording_does_not_count() {
        assert_eq!(run(120, |_| (Some(500), false)), Vec::<u64>::new(), "minimized / covered / locked / display off");
        assert_eq!(run(120, |_| (None, true)), Vec::<u64>::new(), "starting / recovering: not a recording");
        assert_eq!(run(60, |i| (Some(500), i >= 30)), [40], "the count starts when the game is back");
    }
}
