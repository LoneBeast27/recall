//! A "recovering" engine that stopped updating counts as failed, so the last-resort engine
//! restart (`record_recover`) still comes. The engine bounds its own recovery by real
//! errors (it says `failed` after 5 failed reopens), but a hung engine never says it: its
//! status lines stop coming ([`SILENT_AFTER`]).
//!
//! Time in "recovering" alone is no evidence: a static game (a paused menu) sends no frame
//! for the engine to retry with, and the engine rightly waits (`encoder/reopen.rs`). That
//! matches the capture watchdog, which restarts capture only on evidence (thread ended,
//! start failed, restart asked), never on a frame gap.
//!
//! Only time the player can see the game counts (`record_screen`): a minimized or covered
//! game, a locked session or a display that is off sends no frames, and an encoder rebuild
//! waits for a frame.

use std::time::{Duration, Instant};

/// No status line this long while recovering: the engine stopped updating.
pub const SILENT_AFTER: Duration = Duration::from_secs(10);

#[derive(Debug, Default)]
pub struct StaleRecovering {
    /// The last status line count and since when it has not moved (seen time only).
    statuses: Option<(u64, Instant)>,
}

impl StaleRecovering {
    /// `state`: `record_status::session_state`; `statuses`: status lines received so far;
    /// `shown`: the game can be seen (`Sight::shown`). True = treat the session as failed.
    pub fn observe(&mut self, state: &str, statuses: u64, shown: bool, now: Instant) -> bool {
        if state != "recovering" || !shown {
            *self = Self::default();
            return false;
        }
        let quiet_since = match self.statuses {
            Some((n, at)) if n == statuses => at,
            _ => {
                self.statuses = Some((statuses, now));
                now
            }
        };
        now.duration_since(quiet_since) >= SILENT_AFTER
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One look per second `0..=to`: (state, status count, shown); the seconds it says failed.
    fn run(to: u64, at: impl Fn(u64) -> (&'static str, u64, bool)) -> Vec<u64> {
        let t0 = Instant::now();
        let mut w = StaleRecovering::default();
        (0..=to)
            .filter(|&s| {
                let (state, n, shown) = at(s);
                w.observe(state, n, shown, t0 + Duration::from_secs(s))
            })
            .collect()
    }

    #[test]
    fn recovering_with_no_status_for_10_s_is_failed() {
        assert_eq!(run(12, |_| ("recovering", 7, true)), [10, 11, 12]);
    }

    #[test]
    fn a_live_engine_recovering_for_2_minutes_without_frames_is_not_failed() {
        // A static game (paused menu) after an encode error: the engine waits for a frame
        // and keeps sending status lines. That is no evidence of a hang; the engine says
        // `failed` itself after 5 real failed reopens (`encoder/reopen.rs`).
        assert_eq!(run(120, |s| ("recovering", s, true)), Vec::<u64>::new());
    }

    #[test]
    fn nothing_while_the_game_cannot_be_seen_or_the_engine_reports_another_state() {
        assert_eq!(run(120, |_| ("recovering", 7, false)), Vec::<u64>::new(), "minimized / covered / locked / display off");
        assert_eq!(run(120, |_| ("recording", 7, true)), Vec::<u64>::new());
        assert_eq!(run(25, |s| ("recovering", 7, s >= 10)), [20, 21, 22, 23, 24, 25], "counts from the return");
    }
}
