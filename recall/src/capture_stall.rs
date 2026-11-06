//! A recording that stopped: the encoder says it records, but its encoded-frame count has
//! not moved for [`STALL_AFTER`] while the game is in front and can be seen (a game window
//! in front presents every frame, so its capture delivers them). The capture watchdog then
//! restarts the capture, at most once per [`RESTART_EVERY`]; the replay buffer is kept (the
//! encoder reopens on the new device and keeps its ring).
//!
//! Time a minimized or covered game, the lock screen or a display that is off sends no
//! frames does not count, and a monitor capture (the idle desktop) is never "stalled": a
//! still desktop sends no frames either.

use std::time::{Duration, Instant};

/// No new encoded frame this long, with the game in front, is a stall.
pub(crate) const STALL_AFTER: Duration = Duration::from_secs(10);
/// At most one stall restart per this long.
pub(crate) const RESTART_EVERY: Duration = Duration::from_secs(60);

#[derive(Debug, Default)]
pub(crate) struct Stall {
    /// The last count seen and since when it has not moved (counting only seen time).
    last: Option<(u64, Instant)>,
    restarted: Option<Instant>,
}

impl Stall {
    /// `frames`: the encoded-frame count while the encoder records (`None` otherwise);
    /// `watched`: a game window in front that can be seen. True = restart the capture now.
    pub(crate) fn observe(&mut self, frames: Option<u64>, watched: bool, now: Instant) -> bool {
        let (Some(n), true) = (frames, watched) else {
            self.last = None;
            return false;
        };
        let since = match self.last {
            Some((seen, at)) if seen == n => at,
            _ => {
                self.last = Some((n, now));
                return false;
            }
        };
        if now.duration_since(since) < STALL_AFTER || self.restarted.is_some_and(|t| now.duration_since(t) < RESTART_EVERY) {
            return false;
        }
        self.restarted = Some(now);
        self.last = None;
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
    fn a_count_that_stops_for_10_s_in_front_restarts_once_then_at_most_every_60_s() {
        assert_eq!(run(150, |_| (Some(500), true)), [10, 70, 130]);
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
