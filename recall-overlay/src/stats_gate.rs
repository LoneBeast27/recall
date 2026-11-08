//! When the stats poller runs: only while the stats pill can show (stats on,
//! connected, the game in front). A poller already running survives a short
//! [`GRACE`] out of view, so alt-tab flicker does not restart the ETW session.

use std::time::{Duration, Instant};

/// How long a running poller outlives the pill.
pub const GRACE: Duration = Duration::from_secs(10);

#[derive(Debug, Default)]
pub struct StatsGate {
    hidden_since: Option<Instant>,
}

impl StatsGate {
    /// True = the poller should run now: the pill can show, or it showed
    /// less than [`GRACE`] ago and the poller still runs.
    pub fn keep(&mut self, stats_on: bool, can_show: bool, running: bool, now: Instant) -> bool {
        if stats_on && can_show {
            self.hidden_since = None;
            return true;
        }
        if !stats_on || !running {
            self.hidden_since = None;
            return false;
        }
        let since = *self.hidden_since.get_or_insert(now);
        now.saturating_duration_since(since) < GRACE
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nothing_runs_while_the_pill_cannot_show() {
        let now = Instant::now();
        let mut gate = StatsGate::default();
        assert!(!gate.keep(true, false, false, now), "stats on, desktop target: no poller");
        assert!(!gate.keep(true, false, false, now + GRACE * 2));
        assert!(!gate.keep(false, true, false, now), "stats off");
    }

    #[test]
    fn a_running_poller_outlives_a_short_alt_tab_only() {
        let now = Instant::now();
        let mut gate = StatsGate::default();
        assert!(gate.keep(true, true, false, now), "the game in front starts it");
        assert!(gate.keep(true, false, true, now + Duration::from_secs(1)));
        assert!(gate.keep(true, false, true, now + GRACE));
        assert!(!gate.keep(true, false, true, now + Duration::from_secs(1) + GRACE), "dropped after the grace");
        assert!(gate.keep(true, true, false, now + GRACE * 3), "back in front: starts again");
        assert!(gate.keep(true, false, true, now + GRACE * 3 + Duration::from_secs(9)), "the grace starts over");
    }
}
