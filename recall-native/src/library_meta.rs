//! Library bookkeeping that keeps the tiles honest over time: metadata keys moved from file
//! names to paths, the day labels rolling over at midnight, and clips whose length cannot be
//! read counted as known.
use std::time::{Duration, Instant};

use super::LibraryState;
use crate::{
    backend::ClipEntry,
    services::library::{self, LocalStamp},
};

/// How often the clock is read for a new day.
const DAY_CHECK_EVERY: Duration = Duration::from_secs(60);

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

    /// A day group's total counts once every clip in it has a length or is known not to have one.
    pub(super) fn count_failed_durations(&mut self) {
        if self.duration_failed.is_empty() {
            return;
        }
        for group in &mut self.groups {
            group.durations_complete = group.items.iter().all(|&i| self.records[i].duration.is_some() || self.duration_failed.contains(&self.records[i].path));
        }
    }

    /// Once a minute: past midnight, "Today" becomes "Yesterday" without a new listing. True
    /// when the labels changed.
    pub fn roll_day(&mut self) -> bool {
        if self.day_check_at.is_some_and(|at| at.elapsed() < DAY_CHECK_EVERY) {
            return false;
        }
        self.day_check_at = Some(Instant::now());
        self.roll_day_to(library::today())
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
