//! Playback watchdog: notices when the editor says "playing" but the clock has stopped
//! moving, and asks for a restart at the current spot (a few times, then gives up with a
//! message instead of spinning). Pure logic; `EditorState::follow_playback` acts on it.
use std::time::{Duration, Instant};

/// No clock movement for this long while playing counts as a stall.
const STALL_AFTER: Duration = Duration::from_millis(900);
/// More than this many restarts within `RESTART_WINDOW` gives up.
const MAX_RESTARTS: usize = 3;
const RESTART_WINDOW: Duration = Duration::from_secs(10);

#[derive(Debug, PartialEq, Eq)]
pub enum WatchAction {
    Nothing,
    /// Pause, seek to the current position and play again.
    Restart,
    /// Stop and tell the user (restarts did not help).
    GiveUp,
}

#[derive(Default)]
pub struct PlaybackWatch {
    last: Option<(f64, Instant)>,
    restarts: Vec<Instant>,
}

impl PlaybackWatch {
    /// Forget the last position (on play, seek, pause, a new clip).
    pub fn reset(&mut self) {
        self.last = None;
    }

    pub fn observe(&mut self, position: f64, now: Instant) -> WatchAction {
        match self.last {
            Some((seen, since)) if (seen - position).abs() < 1e-4 => {
                if now.duration_since(since) < STALL_AFTER {
                    return WatchAction::Nothing;
                }
                self.last = Some((position, now));
                self.restarts.retain(|t| now.duration_since(*t) < RESTART_WINDOW);
                if self.restarts.len() >= MAX_RESTARTS {
                    self.restarts.clear();
                    return WatchAction::GiveUp;
                }
                self.restarts.push(now);
                WatchAction::Restart
            }
            _ => {
                self.last = Some((position, now));
                WatchAction::Nothing
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn moving_clock_is_fine_and_a_frozen_one_restarts() {
        let t0 = Instant::now();
        let mut watch = PlaybackWatch::default();
        assert_eq!(watch.observe(1.0, t0), WatchAction::Nothing);
        assert_eq!(watch.observe(1.1, t0 + Duration::from_millis(100)), WatchAction::Nothing);
        assert_eq!(watch.observe(1.1, t0 + Duration::from_millis(600)), WatchAction::Nothing);
        assert_eq!(watch.observe(1.1, t0 + Duration::from_millis(1100)), WatchAction::Restart);
        // Measured again from the restart, not from the first sighting.
        assert_eq!(watch.observe(1.1, t0 + Duration::from_millis(1500)), WatchAction::Nothing);
    }

    #[test]
    fn repeated_stalls_give_up() {
        let t0 = Instant::now();
        let mut watch = PlaybackWatch::default();
        let mut actions = Vec::new();
        for step in 0..=4 {
            actions.push(watch.observe(2.0, t0 + Duration::from_millis(1000 * step)));
        }
        assert_eq!(actions.iter().filter(|a| **a == WatchAction::Restart).count(), 3);
        assert!(actions.contains(&WatchAction::GiveUp));
    }

    #[test]
    fn reset_forgets_the_stall_timer() {
        let t0 = Instant::now();
        let mut watch = PlaybackWatch::default();
        watch.observe(3.0, t0);
        watch.reset();
        assert_eq!(watch.observe(3.0, t0 + Duration::from_millis(1000)), WatchAction::Nothing);
    }
}
