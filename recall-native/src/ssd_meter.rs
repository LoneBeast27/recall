//! When the sidebar's SSD row asks for fresh cache totals: after a cache change that marks the
//! totals stale (at most every few seconds), and after new files Recall wrote that do not
//! (thumbnails, copies renamed into place, heads: the cache folder's change notifications,
//! `library::cache_watch`), at most every half minute so a burst of new thumbnails does not walk
//! the cache over and over. With no change it no longer walks once a minute; a long backstop
//! stays. The walk runs on its own thread (`Workers::ssd_usage`).
use std::time::{Duration, Instant};

use crate::{services::storage, worker::Workers};

/// Least time between two walks after a cache change.
const MIN_GAP: Duration = Duration::from_secs(3);
/// Least time between two walks after new files only.
const WRITE_GAP: Duration = Duration::from_secs(30);
/// Longest time between two walks.
// timer-ok: a file growing in place (not renamed into place) raises no counted change; this
// backstop shows it.
const REFRESH: Duration = Duration::from_secs(600);

#[derive(Default)]
pub struct SsdMeter {
    pub usage: Option<storage::SsdUsage>,
    /// (cache generation, new-file count) the last walk saw.
    seen: Option<(u64, u64)>,
    asked: Option<Instant>,
    busy: bool,
}

impl SsdMeter {
    /// `ready`: the first listing is in, so the first walk does not compete with start-up.
    pub fn poll(&mut self, workers: &Workers, ready: bool) {
        if self.busy || !ready {
            return;
        }
        let now = (storage::cache_generation(), storage::cache_writes());
        if due(self.asked.map(|at| at.elapsed()), self.change(now)) {
            self.busy = true;
            self.asked = Some(Instant::now());
            self.seen = Some(now);
            workers.ssd_usage();
        }
    }

    /// When the next walk is due (the UI pass wakes then); a cache change wakes the pass
    /// itself. `None` while a walk runs or before the first listing.
    pub fn next_walk(&self, ready: bool) -> Option<Instant> {
        if self.busy || !ready {
            return None;
        }
        let Some(asked) = self.asked else { return Some(Instant::now()) };
        Some(asked + gap(self.change((storage::cache_generation(), storage::cache_writes()))))
    }

    pub fn set(&mut self, usage: storage::SsdUsage) {
        self.usage = Some(usage);
        self.busy = false;
    }

    fn change(&self, now: (u64, u64)) -> Change {
        match self.seen {
            Some((generation, _)) if generation != now.0 => Change::Stale,
            None => Change::Stale,
            Some((_, writes)) if writes != now.1 => Change::Written,
            Some(_) => Change::None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Change {
    None,
    /// New files only.
    Written,
    /// The totals were marked stale.
    Stale,
}

fn gap(change: Change) -> Duration {
    match change {
        Change::Stale => MIN_GAP,
        Change::Written => WRITE_GAP,
        Change::None => REFRESH,
    }
}

/// `since`: time since the last walk started (`None`: never walked).
fn due(since: Option<Duration>, change: Change) -> bool {
    since.is_none_or(|age| age >= gap(change))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn walks_first_then_after_changes_and_on_the_backstop() {
        assert!(due(None, Change::None), "first poll walks");
        assert!(!due(Some(Duration::from_secs(1)), Change::Stale), "a change waits out the gap");
        assert!(due(Some(MIN_GAP), Change::Stale), "then walks");
        assert!(!due(Some(Duration::from_secs(10)), Change::Written), "new thumbnails wait longer");
        assert!(due(Some(WRITE_GAP), Change::Written));
        assert!(!due(Some(Duration::from_secs(60)), Change::None), "no change: no minute walk any more");
        assert!(due(Some(REFRESH), Change::None), "the backstop walks anyway");
    }

    #[test]
    fn a_new_file_or_a_stale_mark_is_a_change() {
        let meter = SsdMeter { seen: Some((4, 9)), ..Default::default() };
        assert_eq!(meter.change((4, 9)), Change::None);
        assert_eq!(meter.change((4, 10)), Change::Written);
        assert_eq!(meter.change((5, 10)), Change::Stale, "a stale mark wins");
        assert_eq!(SsdMeter::default().change((0, 0)), Change::Stale, "never walked");
    }
}
