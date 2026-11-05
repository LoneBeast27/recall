//! When the sidebar's SSD row asks for fresh cache totals: after a cache change (at most every
//! few seconds) and once a minute otherwise, so writes that do not mark the totals stale (new
//! thumbnails, copies, heads) still show. The walk runs on its own thread (`Workers::ssd_usage`).
use std::time::{Duration, Instant};

use crate::{services::storage, worker::Workers};

/// Least time between two walks after a cache change.
const MIN_GAP: Duration = Duration::from_secs(3);
/// Longest time between two walks.
const REFRESH: Duration = Duration::from_secs(60);

#[derive(Default)]
pub struct SsdMeter {
    pub usage: Option<storage::SsdUsage>,
    seen: Option<u64>,
    asked: Option<Instant>,
    busy: bool,
}

impl SsdMeter {
    /// `ready`: the first listing is in, so the first walk does not compete with start-up.
    pub fn poll(&mut self, workers: &Workers, ready: bool) {
        if self.busy || !ready {
            return;
        }
        let generation = storage::cache_generation();
        if due(self.asked.map(|at| at.elapsed()), self.seen != Some(generation)) {
            self.busy = true;
            self.asked = Some(Instant::now());
            self.seen = Some(generation);
            workers.ssd_usage();
        }
    }

    pub fn set(&mut self, usage: storage::SsdUsage) {
        self.usage = Some(usage);
        self.busy = false;
    }
}

/// `since`: time since the last walk started (`None`: never walked).
fn due(since: Option<Duration>, changed: bool) -> bool {
    match since {
        None => true,
        Some(age) => age >= REFRESH || (changed && age >= MIN_GAP),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn walks_first_then_after_changes_and_once_a_minute() {
        assert!(due(None, false), "first poll walks");
        assert!(!due(Some(Duration::from_secs(1)), true), "a change waits out the gap");
        assert!(due(Some(MIN_GAP), true), "then walks");
        assert!(!due(Some(Duration::from_secs(30)), false), "no change: no walk");
        assert!(due(Some(REFRESH), false), "a minute on, it walks anyway");
    }
}
