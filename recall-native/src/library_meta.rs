//! Library bookkeeping that keeps the tiles honest over time: metadata keys moved from file
//! names to paths, the day labels rolling over at midnight, and clips whose length cannot be
//! read counted as known.
use std::time::{Duration, Instant};

use super::LibraryState;
use crate::{
    backend::ClipEntry,
    services::library::{self, LocalStamp},
};

/// Longest wait between two looks at the clock for a new day.
// timer-ok: a monotonic timer to midnight drifts when the PC sleeps or the clock is set; resume
// and clock changes wake it (`os_events::Topic::Clock`), this is the backstop.
const DAY_CHECK_MAX: Duration = Duration::from_secs(3600);

impl LibraryState {
    /// Moves file-name keyed metadata onto the clips' paths: every listed clip (`None`), or the
    /// clips the watcher just added. The move is flushed with the next duration flush.
    pub(super) fn migrate_meta_keys(&mut self, added: Option<&[ClipEntry]>) {
        let entries = added.unwrap_or(&self.entries);
        let moved = self.store.migrate_names(entries.iter().map(|e| (e.path.as_str(), e.name.as_str())));
        if moved {
            self.meta_dirty_since.get_or_insert_with(Instant::now);
        }
    }

    /// Clips an earlier run could not read a length for (same size and mtime): shown as "—"
    /// at once and not queued for another probe.
    pub(super) fn recall_failed_durations(&mut self) {
        for record in self.records.iter().filter(|r| r.duration.is_none()) {
            if self.store.duration_unknown(&record.path, &record.file_name, record.bytes, record.mtime) {
                self.duration_failed.insert(record.path.clone());
                self.duration_queued.insert(record.path.clone());
            }
        }
    }

    /// A day group's total counts once every clip in it has a length or is known not to have one.
    pub(super) fn count_failed_durations(&mut self) {
        if self.duration_failed.is_empty() {
            return;
        }
        for group in &mut self.groups {
            group.durations_complete = group.items.iter().all(|&i| self.records[i].duration.is_some() || self.duration_failed.contains(&self.records[i].path));
        }
    }

    /// At local midnight (and after a clock change or a wake from sleep): "Today" becomes
    /// "Yesterday" without a new listing. True when the labels changed.
    pub fn roll_day(&mut self) -> bool {
        let clock = crate::services::os_events::generation(crate::services::os_events::Topic::Clock);
        if self.day_check_at.is_some_and(|at| Instant::now() < at) && clock == self.day_clock {
            return false;
        }
        self.day_clock = clock;
        let today = library::today();
        self.day_check_at = Some(Instant::now() + next_day_check(&today));
        self.roll_day_to(today)
    }

    /// When the library's own upkeep is due (the UI pass wakes then): the debounced metadata
    /// save, the next look at the clock for a new day.
    pub fn upkeep_deadlines(&self) -> [Option<Instant>; 2] {
        [self.meta_dirty_since.map(|since| since + super::META_FLUSH_AFTER), self.day_check_at]
    }

    pub(super) fn roll_day_to(&mut self, today: LocalStamp) -> bool {
        if today.date_key() == self.today.date_key() {
            self.today = today;
            return false;
        }
        self.today = today;
        self.refilter();
        true
    }
}

/// Wait from `now` (local wall clock) to just past the next local midnight, at most
/// [`DAY_CHECK_MAX`].
fn next_day_check(now: &LocalStamp) -> Duration {
    let into_day = u64::from(now.hour) * 3600 + u64::from(now.minute) * 60 + u64::from(now.second);
    Duration::from_secs(86_400u64.saturating_sub(into_day) + 1).min(DAY_CHECK_MAX)
}

#[cfg(test)]
mod day_tests {
    use super::*;

    #[test]
    fn the_day_check_lands_just_past_midnight() {
        let at = |hour, minute, second| LocalStamp { year: 2026, month: 11, day: 13, hour, minute, second };
        assert_eq!(next_day_check(&at(23, 59, 30)), Duration::from_secs(31), "30 s to midnight, one past it");
        assert_eq!(next_day_check(&at(23, 30, 0)), Duration::from_secs(1801));
        assert_eq!(next_day_check(&at(9, 0, 0)), DAY_CHECK_MAX, "far from midnight: the hourly backstop");
        assert_eq!(next_day_check(&at(0, 0, 0)), DAY_CHECK_MAX);
    }
}
