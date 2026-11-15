//! When a watcher may try a running app's capture again after it failed (the process could
//! not be opened, process loopback refused it): never at once, but after 2, 5, 10, then every
//! 30 s while it keeps failing for that same process. A new process (another pid) is tried at
//! once; a capture that worked clears it.

use std::time::{Duration, Instant};

/// timer-ok: a capture that failed has no event that says it would work now.
const BACKOFF: [Duration; 4] = [Duration::from_secs(2), Duration::from_secs(5), Duration::from_secs(10), Duration::from_secs(30)];

#[derive(Debug, Default)]
pub(super) struct Retry {
    /// The pid that failed, how often in a row, and when it may be tried again.
    failed: Option<(u32, usize, Instant)>,
}

impl Retry {
    /// `pid` may be captured now.
    pub(super) fn allows(&self, pid: u32, now: Instant) -> bool {
        self.failed.is_none_or(|(failed, _, at)| failed != pid || now >= at)
    }

    /// A failed pid's wait is over: the watcher looks whether it still runs.
    pub(super) fn due(&self, now: Instant) -> bool {
        self.failed.is_some_and(|(_, _, at)| now >= at)
    }

    /// The capture of `pid` failed at `now`.
    pub(super) fn failed(&mut self, pid: u32, now: Instant) {
        let n = match self.failed {
            Some((same, n, _)) if same == pid => n + 1,
            _ => 0,
        };
        self.failed = Some((pid, n, now + BACKOFF[n.min(BACKOFF.len() - 1)]));
    }

    /// The capture worked, or the failed process is gone.
    pub(super) fn clear(&mut self) {
        self.failed = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Gate re-review P2: a capture that fails is never retried in a tight loop.
    #[test]
    fn a_failing_capture_backs_off_2_5_10_then_30_s() {
        let t0 = Instant::now();
        let s = Duration::from_secs;
        let mut r = Retry::default();
        assert!(r.allows(7, t0) && !r.due(t0), "nothing failed yet");
        let mut at = t0;
        for wait in [2, 5, 10, 30, 30, 30] {
            r.failed(7, at);
            assert!(!r.allows(7, at), "not at once");
            assert!(!r.allows(7, at + s(wait) - Duration::from_millis(1)), "not before {wait} s");
            assert!(r.allows(7, at + s(wait)) && r.due(at + s(wait)), "after {wait} s");
            at += s(wait);
        }
        assert!(r.allows(8, at), "a new process is tried at once");
        r.failed(8, at);
        assert!(r.allows(8, at + s(2)), "and starts its own backoff at 2 s");
        r.clear();
        assert!(r.allows(8, at) && !r.due(at), "a capture that worked clears it");
    }
}
