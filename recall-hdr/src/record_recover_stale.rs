//! A "recovering" engine whose recovery stopped moving counts as failed, so the last-resort
//! engine restart (`record_recover`) still comes. The engine bounds its own recovery (it
//! says `failed` after 5 reopens or 60 s), but a hung engine never says it: its status
//! lines stop coming ([`SILENT_AFTER`]), or the status says "recovering" past the engine's
//! own bound ([`RECOVERING_TOO_LONG`]).
//!
//! Only time the player can see the game counts (`record_screen`): a minimized or covered
//! game, a locked session or a display that is off sends no frames, and an encoder rebuild
//! waits for a frame.

use std::time::{Duration, Instant};

/// No status line this long while recovering: the engine stopped updating.
pub const SILENT_AFTER: Duration = Duration::from_secs(10);
/// "recovering" this long: past the engine's own 60 s bound.
pub const RECOVERING_TOO_LONG: Duration = Duration::from_secs(60);

#[derive(Debug, Default)]
pub struct StaleRecovering {
    /// Since when the session has said "recovering" (seen time only).
    since: Option<Instant>,
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
        let since = *self.since.get_or_insert(now);
        let quiet_since = match self.statuses {
            Some((n, at)) if n == statuses => at,
            _ => {
                self.statuses = Some((statuses, now));
                now
            }
        };
        now.duration_since(quiet_since) >= SILENT_AFTER || now.duration_since(since) >= RECOVERING_TOO_LONG
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
    fn recovering_past_the_engines_60_s_bound_is_failed_though_statuses_still_come() {
        assert_eq!(run(61, |s| ("recovering", s, true)), [60, 61]);
    }

    #[test]
    fn nothing_while_the_game_cannot_be_seen_or_the_engine_reports_another_state() {
        assert_eq!(run(120, |_| ("recovering", 7, false)), Vec::<u64>::new(), "minimized / covered / locked / display off");
        assert_eq!(run(120, |_| ("recording", 7, true)), Vec::<u64>::new());
        assert_eq!(run(25, |s| ("recovering", 7, s >= 10)), [20, 21, 22, 23, 24, 25], "counts from the return");
    }
}
