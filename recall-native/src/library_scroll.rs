//! How far back in time the user browses the Library, for the smart cache.
use super::{LibraryState, Row, interaction};
use crate::services::library::LocalStamp;

impl LibraryState {
    /// Records how far back in time the user browses (deepest day group reached), so the
    /// smart cache knows how much history is worth keeping warm.
    pub fn note_scroll_depth(&mut self) {
        let top = self.list.logical_scroll_top().item_ix;
        let group = match self.rows.get(top) {
            Some(Row::Header { group, .. }) => *group,
            Some(Row::Tiles(items)) => self.groups.iter().position(|g| items.first().is_some_and(|i| g.items.contains(i))).unwrap_or(0),
            None => return,
        };
        let Some(first) = self.groups.get(group).and_then(|g| g.items.first()).map(|i| self.records[*i].started) else { return };
        let days = days_between(first, self.today);
        if days > self.deepest_scroll_days {
            self.deepest_scroll_days = days;
            interaction(crate::services::storage::Interaction::ScrolledTo { days_back: days });
        }
    }
}

/// Whole days from `earlier` to `later` (local calendar dates).
fn days_between(earlier: LocalStamp, later: LocalStamp) -> u32 {
    fn ordinal(s: LocalStamp) -> i64 {
        let (y, m, d) = (s.year as i64, s.month as i64, s.day as i64);
        let (y, m) = if m <= 2 { (y - 1, m + 12) } else { (y, m) };
        365 * y + y / 4 - y / 100 + y / 400 + (153 * (m - 3) + 2) / 5 + d
    }
    (ordinal(later) - ordinal(earlier)).max(0) as u32
}
