//! The last played clip: opening a clip stamps it, the Library offers it back (Resume button,
//! "Last played" tile badge) and scrolls to its tile when the editor closes.
use std::time::{SystemTime, UNIX_EPOCH};

use super::{LibraryState, Row};
use crate::services::library::ClipRecord;

impl LibraryState {
    /// Stamps the clip as opened now. Resolves `\\?\` editor paths to the listed clip.
    pub fn mark_played(&mut self, path: &str) {
        let now = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs());
        self.mark_played_at(path, now);
    }

    pub(super) fn mark_played_at(&mut self, path: &str, secs: u64) {
        let Some(listed) = self.record(path).map(|r| r.path.clone()) else { return };
        let Some(index) = self.by_path.get(&listed).copied() else { return };
        let name = self.records[index].file_name.clone();
        match self.store.set_played(&name, secs) {
            Ok(()) => {
                self.records[index].played = Some(secs);
                self.played = latest(&self.records);
            }
            Err(error) => self.notice = Some(format!("Couldn't save last played: {error}")),
        }
    }

    /// The clip opened most recently, whatever the filter shows.
    pub fn last_played(&self) -> Option<&ClipRecord> {
        self.played.as_deref().and_then(|path| self.by_path.get(path)).and_then(|i| self.records.get(*i))
    }

    /// Scrolls the grid so the clip's tile is in view; nothing happens when it is filtered out.
    pub fn reveal(&self, path: &str) {
        if let Some(row) = self.row_of(path) {
            self.list.scroll_to_reveal_item(row);
        }
    }

    /// The tile row that holds the clip.
    pub(super) fn row_of(&self, path: &str) -> Option<usize> {
        let index = self.by_path.get(&self.record(path)?.path).copied()?;
        self.rows.iter().position(|row| matches!(row, Row::Tiles(items) if items.contains(&index)))
    }
}

/// Path of the record with the latest `played` stamp.
pub(super) fn latest(records: &[ClipRecord]) -> Option<String> {
    records.iter().filter(|r| r.played.is_some()).max_by_key(|r| r.played).map(|r| r.path.clone())
}
