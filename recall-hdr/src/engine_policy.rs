//! The capture-engine restart policy (pure; the tested core of the supervisor)
//! and the per-spawn engine spec.
//!
//! On an UNINTENDED child exit we back off 500ms, 1s, 2s, 4s, 8s, then cap at
//! 30s. If a child stays up longer than 60s the backoff resets to 500ms. To keep
//! a broken/missing engine from fork-bombing, we give up after 6 failures inside
//! any rolling 2-minute window.
//!
//! INTENDED restarts (a retarget to a new game, a settings change) are not
//! failures: they never count toward give-up and never back off. A new target
//! also clears the circuit breaker, so a game that crashed the engine does not
//! leave the next game unrecorded.

use std::time::{Duration, Instant};

/// Base backoff before the first restart; doubles each attempt up to the cap.
const BACKOFF_BASE_MS: u64 = 500;
/// Ceiling on the restart backoff -- a crash-looping engine retries at most this often.
const BACKOFF_CAP_MS: u64 = 30_000;
/// A child that stays up at least this long is "healthy" -- reset the backoff.
pub const STABLE_UPTIME: Duration = Duration::from_secs(60);
/// The longest wait between retries while another engine holds the recorder.
pub const BUSY_RETRY_MAX: Duration = Duration::from_secs(5);
/// Give-up window: this many failures inside it trips the circuit breaker.
pub const GIVEUP_WINDOW: Duration = Duration::from_secs(120);
/// Failures within `GIVEUP_WINDOW` that trip give-up (anti fork-bomb).
pub const GIVEUP_THRESHOLD: usize = 6;

/// The engine's exit code for "another engine already records" (`recall/src/main.rs`).
pub const EXIT_SLOT_BUSY: i32 = 3;

/// Environment edits for one engine spawn: `Some` sets, `None` removes. The
/// default (empty) spec inherits the controller's environment unchanged.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EngineSpec {
    pub env: Vec<(String, Option<String>)>,
}

/// Restart delay for the Nth consecutive failed attempt (0-based).
///
/// `500ms, 1s, 2s, 4s, 8s, ...` doubling, capped at 30s. Saturating so a large
/// attempt count can never overflow the shift.
pub fn backoff_delay(attempt: u32) -> Duration {
    let ms = BACKOFF_BASE_MS
        .checked_shl(attempt)
        .map(|v| v.min(BACKOFF_CAP_MS))
        .unwrap_or(BACKOFF_CAP_MS);
    Duration::from_millis(ms)
}

/// Circuit breaker: should we give up given the failure history? We give up
/// once at least [`GIVEUP_THRESHOLD`] failures fall inside the trailing
/// [`GIVEUP_WINDOW`].
pub fn should_give_up(failures: &[Instant], now: Instant) -> bool {
    let recent = failures
        .iter()
        .filter(|&&t| now.duration_since(t) <= GIVEUP_WINDOW)
        .count();
    recent >= GIVEUP_THRESHOLD
}

/// What to do after an unintended exit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Next {
    /// Wait this long, then respawn (shown as `Restarting { attempt }`).
    Backoff { delay: Duration, attempt: u32 },
    /// Circuit breaker tripped: park until the next retarget.
    GiveUp,
}

/// Failure accounting across engine lifetimes.
#[derive(Debug, Default)]
pub struct RestartPolicy {
    attempt: u32,
    failures: Vec<Instant>,
    /// Retries in a row while another engine holds the recorder (not failures).
    busy: u32,
}

impl RestartPolicy {
    /// The child died (or failed to spawn) without being asked to.
    pub fn on_unintended_exit(&mut self, now: Instant, uptime: Duration) -> Next {
        // A child that stayed up long enough was healthy: start backoff fresh.
        if uptime >= STABLE_UPTIME {
            self.attempt = 0;
        }
        self.failures.push(now);
        self.failures.retain(|&t| now.duration_since(t) <= GIVEUP_WINDOW);
        if should_give_up(&self.failures, now) {
            return Next::GiveUp;
        }
        let delay = backoff_delay(self.attempt);
        self.attempt = self.attempt.saturating_add(1);
        Next::Backoff { delay, attempt: self.attempt }
    }

    /// An engine exit (`code`: its exit code, None without one) the controller did not ask for.
    /// [`EXIT_SLOT_BUSY`] means another engine still holds the recorder (an orphan of a
    /// crashed controller finishing its save): not a failure.
    pub fn on_exit(&mut self, code: Option<i32>, now: Instant, uptime: Duration) -> Next {
        if code == Some(EXIT_SLOT_BUSY) {
            let delay = backoff_delay(self.busy).min(BUSY_RETRY_MAX);
            self.busy = self.busy.saturating_add(1);
            return Next::Backoff { delay, attempt: self.busy };
        }
        self.busy = 0;
        self.on_unintended_exit(now, uptime)
    }

    /// The controller restarted the engine on purpose: deliberately NOT a
    /// failure (no give-up count, no backoff).
    pub fn on_intended_restart(&mut self) {}

    /// A new target: forget the old target's failures.
    pub fn reset(&mut self) {
        self.attempt = 0;
        self.busy = 0;
        self.failures.clear();
    }

    pub fn recent_failures(&self) -> usize {
        self.failures.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- backoff sequence -------------------------------------------------

    #[test]
    fn backoff_sequence_doubles_then_caps() {
        assert_eq!(backoff_delay(0), Duration::from_millis(500));
        assert_eq!(backoff_delay(1), Duration::from_millis(1_000));
        assert_eq!(backoff_delay(2), Duration::from_millis(2_000));
        assert_eq!(backoff_delay(3), Duration::from_millis(4_000));
        assert_eq!(backoff_delay(4), Duration::from_millis(8_000));
        // 500 * 2^5 = 16s (still under cap), 2^6 = 32s -> capped at 30s.
        assert_eq!(backoff_delay(5), Duration::from_millis(16_000));
        assert_eq!(backoff_delay(6), Duration::from_millis(30_000));
        assert_eq!(backoff_delay(7), Duration::from_millis(30_000));
    }

    #[test]
    fn backoff_never_overflows_on_huge_attempt() {
        assert_eq!(backoff_delay(1_000), Duration::from_millis(30_000));
        assert_eq!(backoff_delay(u32::MAX), Duration::from_millis(30_000));
    }

    // --- give-up window ---------------------------------------------------

    #[test]
    fn give_up_after_six_failures_in_window() {
        let now = Instant::now();
        let fails: Vec<Instant> = (0..6).map(|i| now - Duration::from_secs(i * 5)).collect();
        assert!(should_give_up(&fails, now));
    }

    #[test]
    fn do_not_give_up_below_threshold() {
        let now = Instant::now();
        let fails: Vec<Instant> = (0..5).map(|i| now - Duration::from_secs(i * 5)).collect();
        assert!(!should_give_up(&fails, now), "5 failures is under the threshold");
    }

    #[test]
    fn old_failures_fall_out_of_window() {
        let now = Instant::now();
        let mut fails: Vec<Instant> = (1..=5).map(|i| now - Duration::from_secs(120 + i * 10)).collect();
        fails.push(now - Duration::from_secs(1));
        assert!(!should_give_up(&fails, now), "stale failures outside the 2-min window shouldn't count");
    }

    #[test]
    fn six_recent_among_many_old_trips_giveup() {
        let now = Instant::now();
        let mut fails: Vec<Instant> = (0..10).map(|i| now - Duration::from_secs(300 + i * 5)).collect();
        for i in 0..6 {
            fails.push(now - Duration::from_secs(i * 10));
        }
        assert!(should_give_up(&fails, now));
    }

    // --- intended-restart accounting --------------------------------------

    #[test]
    fn intended_restarts_never_count_toward_give_up_or_backoff() {
        let t0 = Instant::now();
        let mut p = RestartPolicy::default();
        // Fifty game switches in a minute: none are failures.
        for _ in 0..50 {
            p.on_intended_restart();
        }
        assert_eq!(p.recent_failures(), 0);
        // The first real crash afterwards still starts at the 500ms base.
        assert_eq!(
            p.on_unintended_exit(t0, Duration::from_secs(1)),
            Next::Backoff { delay: Duration::from_millis(500), attempt: 1 }
        );
    }

    #[test]
    fn six_unintended_exits_in_window_give_up_and_reset_clears_it() {
        let t0 = Instant::now();
        let mut p = RestartPolicy::default();
        for i in 0..5 {
            assert!(matches!(p.on_unintended_exit(t0 + Duration::from_secs(i), Duration::ZERO), Next::Backoff { .. }));
        }
        assert_eq!(p.on_unintended_exit(t0 + Duration::from_secs(5), Duration::ZERO), Next::GiveUp);
        p.reset(); // a new target gets a fresh breaker
        assert_eq!(p.recent_failures(), 0);
        assert!(matches!(p.on_unintended_exit(t0 + Duration::from_secs(6), Duration::ZERO), Next::Backoff { attempt: 1, .. }));
    }

    #[test]
    fn an_engine_slot_still_held_by_an_orphan_never_gives_up() {
        // R6-overlay-2: the old controller's orphan finishes its save while the new
        // controller's engine exits 3 on every try.
        let t0 = Instant::now();
        let mut p = RestartPolicy::default();
        for i in 0..20 {
            let next = p.on_exit(Some(EXIT_SLOT_BUSY), t0 + Duration::from_secs(i * 6), Duration::ZERO);
            match next {
                Next::Backoff { delay, .. } => assert!(delay <= BUSY_RETRY_MAX, "retry {i}: {delay:?}"),
                Next::GiveUp => panic!("busy exit {i} gave up"),
            }
        }
        assert_eq!(p.recent_failures(), 0, "busy exits are not failures");
        // Once the slot frees, a real crash starts the backoff at its base.
        assert_eq!(
            p.on_exit(Some(1), t0 + Duration::from_secs(200), Duration::ZERO),
            Next::Backoff { delay: Duration::from_millis(500), attempt: 1 }
        );
        assert!(matches!(p.on_exit(None, t0 + Duration::from_secs(201), Duration::ZERO), Next::Backoff { attempt: 2, .. }));
    }

    #[test]
    fn stable_uptime_resets_backoff_but_not_the_window() {
        let t0 = Instant::now();
        let mut p = RestartPolicy::default();
        p.on_unintended_exit(t0, Duration::ZERO);
        p.on_unintended_exit(t0 + Duration::from_secs(1), Duration::ZERO);
        let next = p.on_unintended_exit(t0 + Duration::from_secs(2), STABLE_UPTIME);
        assert_eq!(next, Next::Backoff { delay: Duration::from_millis(500), attempt: 1 });
        assert_eq!(p.recent_failures(), 3);
    }
}
