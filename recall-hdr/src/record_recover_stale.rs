//! What the engine's own status cannot say: that it hung, or that a recovery an error
//! started is getting nowhere. Both feed `record_recover`, the last-resort engine restart.
//!
//! - **Hung** ([`HUNG_AFTER`]): the engine's encoder heartbeat (`status.encoder_heartbeat`,
//!   one beat per encoder-thread turn, frames or not) stopped moving while the session is
//!   `recording`, `degraded` or `recovering`. An engine without the field is judged by its
//!   status lines instead, so this is also the old silent-engine rule: when status lines
//!   stop, the beat seen here stops with them (one rule, not two). A static game (a paused
//!   menu) keeps the beat; an NVENC call hung after a TDR stops it, also under a status
//!   that still says `recording`. While the engine says an NVENC open is in progress
//!   (`status.encoder_opening`; a TDR open can take about 10 s) the clock allows
//!   [`HUNG_OPENING_AFTER`]. One restart per hang: another only after the engine has beaten
//!   again for [`HEALTHY_AFTER`]; a second hang before that is [`Verdict::HungAgain`]
//!   (once), and `record_recover` gives up.
//! - **Failed** ([`RECOVERING_BOUND`]): `recovering` for 90 s of visible time while the
//!   engine still beats. `recovering` only exists inside an episode a real open or encode
//!   error started (`encoder/reopen.rs`), so a static game cannot trip it; it covers WGC
//!   going quiet after device-removed with no watchdog evidence. A hung engine is the
//!   hang rule's, not this one's.
//!
//! Only time the player can see the game counts (`record_screen`): a minimized or covered
//! game, a locked session or a display that is off gets neither rule. The capture
//! watchdog's own rule stays evidence-only (thread ended, start failed, restart asked).

use std::time::{Duration, Instant};

/// No heartbeat (or, on an older engine, no status line) this long: the engine hung.
pub const HUNG_AFTER: Duration = Duration::from_secs(10);
/// The same while an NVENC open is in progress.
pub const HUNG_OPENING_AFTER: Duration = Duration::from_secs(20);
/// A hang restart is allowed again once the engine has beaten this long.
pub const HEALTHY_AFTER: Duration = Duration::from_secs(60);
/// "recovering" this long of visible time, the engine beating: count it as failed.
pub const RECOVERING_BOUND: Duration = Duration::from_secs(90);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    /// Restart the engine now, once.
    Hung,
    /// Hung again before a healthy minute after the hang restart (said once).
    HungAgain,
    /// Treat the session as `error` (the usual grace and backoff apply).
    Failed,
}

#[derive(Debug, Default)]
pub struct EngineWatch {
    /// The last progress value and when it last changed (seen time).
    progress: Option<(u64, Instant)>,
    /// Since when the engine has been beating without a stall.
    beating_since: Option<Instant>,
    /// This hang has had its restart.
    hang_restarted: bool,
    /// [`Verdict::HungAgain`] was said for it.
    again_said: bool,
    /// Visible time in "recovering", and the last visible look at it.
    recovering_seen: Duration,
    last_look: Option<Instant>,
}

impl EngineWatch {
    /// `state`: `record_status::session_state`; `progress`: the encoder heartbeat (or the
    /// status line count); `opening`: an NVENC open is in progress; `shown`: the game can
    /// be seen (`Sight::shown`).
    pub fn observe(&mut self, state: &str, progress: u64, opening: bool, shown: bool, now: Instant) -> Option<Verdict> {
        self.bound_clock(state, shown, now);
        if !matches!(state, "recording" | "degraded" | "recovering") || !shown {
            self.progress = None;
            self.beating_since = None;
            return None;
        }
        let stalled = match self.progress {
            Some((value, at)) if value == progress => now.duration_since(at) >= if opening { HUNG_OPENING_AFTER } else { HUNG_AFTER },
            _ => {
                self.progress = Some((progress, now));
                let since = *self.beating_since.get_or_insert(now);
                if now.duration_since(since) >= HEALTHY_AFTER {
                    self.forget_hangs();
                }
                false
            }
        };
        if stalled {
            self.beating_since = None;
            if !self.hang_restarted {
                return Some(Verdict::Hung);
            }
            return (!std::mem::replace(&mut self.again_said, true)).then_some(Verdict::HungAgain);
        }
        (state == "recovering" && self.recovering_seen >= RECOVERING_BOUND).then_some(Verdict::Failed)
    }

    /// The engine has beaten without a stall for [`HEALTHY_AFTER`] (it works again).
    pub fn healthy(&self, now: Instant) -> bool {
        self.beating_since.is_some_and(|since| now.duration_since(since) >= HEALTHY_AFTER)
    }

    /// A new game or new settings: the next hang gets its restart again.
    pub fn forget_hangs(&mut self) {
        self.hang_restarted = false;
        self.again_said = false;
    }

    /// The hang verdict was acted on (the engine restarts).
    pub fn hang_restart_done(&mut self) {
        self.hang_restarted = true;
        self.progress = None;
    }

    fn bound_clock(&mut self, state: &str, shown: bool, now: Instant) {
        if state != "recovering" {
            self.recovering_seen = Duration::ZERO;
            self.last_look = None;
            return;
        }
        if !shown {
            self.last_look = None;
            return;
        }
        if let Some(prev) = self.last_look {
            self.recovering_seen += now.saturating_duration_since(prev);
        }
        self.last_look = Some(now);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One look per second `0..=to`: (state, progress, shown); the verdicts by second.
    fn run(to: u64, at: impl Fn(u64) -> (&'static str, u64, bool)) -> Vec<(u64, Verdict)> {
        let t0 = Instant::now();
        let mut w = EngineWatch::default();
        (0..=to)
            .filter_map(|s| {
                let (state, n, shown) = at(s);
                let v = w.observe(state, n, false, shown, t0 + Duration::from_secs(s))?;
                if v == Verdict::Hung {
                    w.hang_restart_done();
                }
                Some((s, v))
            })
            .collect()
    }

    #[test]
    fn a_stalled_beat_is_one_hang_and_an_older_engines_silent_status_is_the_same_rule() {
        // `progress` is the heartbeat, or the status line count on an engine without one.
        assert_eq!(run(60, |_| ("recovering", 7, true)), [(10, Verdict::Hung), (21, Verdict::HungAgain)]);
        assert_eq!(run(60, |s| ("degraded", s.min(5), true)), [(15, Verdict::Hung), (26, Verdict::HungAgain)]);
    }

    #[test]
    fn nothing_while_the_game_cannot_be_seen_or_the_engine_is_starting() {
        assert_eq!(run(200, |_| ("recovering", 7, false)), []);
        assert_eq!(run(200, |_| ("starting", 7, true)), []);
        assert_eq!(run(25, |s| ("recording", 7, s >= 10)), [(20, Verdict::Hung)], "counts from the return");
        let t0 = Instant::now();
        let mut w = EngineWatch::default();
        let opening: Vec<u64> = (0..=30).filter(|&s| w.observe("recovering", 7, true, true, t0 + Duration::from_secs(s)).is_some()).collect();
        assert_eq!(opening.first(), Some(&20), "an open in progress gets 20 s");
    }

    #[test]
    fn a_beating_engine_recovering_for_90_s_visible_is_failed() {
        let failed: Vec<u64> = run(95, |s| ("recovering", s, true)).into_iter().map(|(s, _)| s).collect();
        assert_eq!(failed, [90, 91, 92, 93, 94, 95]);
        assert_eq!(run(300, |s| ("recording", s, true)), [], "a beating static game");
    }
}
