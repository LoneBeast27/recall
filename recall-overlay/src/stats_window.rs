//! "Brief" stats (`overlay.stats_show = brief`): any window over a game, even this small pill,
//! makes Windows compose the game's frames instead of letting it flip straight to the screen,
//! which costs it frames. So the stats pill shows for [`BRIEF`] when something happens (the
//! game comes to the front, stats are turned on, a notice ends) and then hides. Checked on the
//! island's existing base tick; nothing runs per frame.

use std::time::{Duration, Instant};

pub const BRIEF: Duration = Duration::from_secs(8);

#[derive(Debug, Default)]
pub struct StatsWindow {
    until: Option<Instant>,
    was_in_game: bool,
    was_on: bool,
    had_notice: bool,
}

impl StatsWindow {
    /// Whether the stats pill may show now. `always`: the user keeps it on screen.
    pub fn open(&mut self, always: bool, stats_on: bool, in_game: bool, notice: bool, now: Instant) -> bool {
        let opened = (in_game && !self.was_in_game) || (stats_on && !self.was_on) || (self.had_notice && !notice);
        (self.was_in_game, self.was_on, self.had_notice) = (in_game, stats_on, notice);
        if opened {
            self.until = Some(now + BRIEF);
        }
        always || self.until.is_some_and(|until| now < until)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_pill_shows_briefly_when_the_game_comes_to_the_front_then_hides() {
        let t0 = Instant::now();
        let mut w = StatsWindow::default();
        assert!(!w.open(false, false, false, false, t0), "stats off, no game in front");
        assert!(w.open(false, true, true, false, t0 + Duration::from_secs(1)), "the game came to the front (with stats on)");
        assert!(w.open(false, true, true, false, t0 + Duration::from_secs(8)));
        assert!(!w.open(false, true, true, false, t0 + Duration::from_secs(10)), "hidden after the brief window");
        assert!(w.open(true, true, true, false, t0 + Duration::from_secs(10)), "always: it stays");
    }

    #[test]
    fn a_notice_ending_or_stats_turned_on_show_it_again() {
        let t0 = Instant::now();
        let mut w = StatsWindow::default();
        w.open(false, true, true, false, t0);
        assert!(!w.open(false, true, true, true, t0 + Duration::from_secs(20)), "a notice is up (the notice shows, not the stats)");
        assert!(w.open(false, true, true, false, t0 + Duration::from_secs(22)), "the notice ended: stats for a moment");
        assert!(!w.open(false, false, true, false, t0 + Duration::from_secs(40)));
        assert!(w.open(false, true, true, false, t0 + Duration::from_secs(41)), "stats turned on");
    }
}
