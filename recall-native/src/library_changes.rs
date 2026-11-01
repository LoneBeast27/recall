//! Folder-watcher changes applied to the library view model: no folder rescan and the scroll
//! position stays on the clip it was on.
use std::{
    collections::{HashMap, HashSet},
    path::Path,
    time::{Duration, Instant},
};

use gpui::{ListOffset, Pixels};

use super::{LibraryState, Row};
use crate::{
    backend::ClipEntry,
    services::library::{self, folders::folder_key, watch::ClipChanges},
};

/// How long a change is replayed over listings: one that started before the change may still
/// be running (a large imported tree takes a while).
const RECENT_KEEP: Duration = Duration::from_secs(600);

/// Lowercase path → when it changed and what it is now (`None`: gone).
pub(super) type Recent = HashMap<String, (Instant, Option<ClipEntry>)>;

/// The anchor clip, its row's distance from the top row, and the pixel offset into the top row.
pub(super) struct ScrollAnchor {
    path: Option<String>,
    top: usize,
    below_top: usize,
    offset: Pixels,
}

impl LibraryState {
    /// Sets Edited on the clip whose edit sidecar was just written or removed.
    pub fn mark_edited_by_sidecar(&mut self, sidecar: &str, edited: bool) {
        let name = Path::new(sidecar).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let Some(stem) = name.strip_suffix(".recall-edit.json").map(|s| s.trim_end_matches(".mp4").to_owned()) else { return };
        for record in &mut self.records {
            let file_stem = Path::new(&record.file_name).file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
            if file_stem == stem {
                record.edited = edited;
            }
        }
    }

    pub fn apply_changes(&mut self, changes: ClipChanges) {
        let anchor = self.scroll_anchor();
        let now = Instant::now();
        self.recent.retain(|_, (at, _)| now.duration_since(*at) < RECENT_KEEP);
        self.remove_gone(&changes, now);
        let mut index: HashMap<String, usize> = HashMap::new();
        if !changes.added.is_empty() {
            index = self.entries.iter().enumerate().map(|(i, e)| (folder_key(&e.path), i)).collect();
        }
        for clip in changes.added {
            let key = folder_key(&clip.entry.path);
            self.recent.insert(key.clone(), (now, Some(clip.entry.clone())));
            let same = |e: &ClipEntry| e.path == clip.entry.path && e.bytes == clip.entry.bytes && e.mtime == clip.entry.mtime;
            let at = index.get(&key).copied();
            if at.is_some_and(|i| same(&self.entries[i])) {
                continue; // Recall's own save: the listing already made this tile.
            }
            self.forget_assets(&clip.entry.path);
            let record = library::build_record(&clip.entry, clip.edited, &self.store);
            match at {
                Some(i) => {
                    self.forget_assets(&self.entries[i].path.clone());
                    self.entries[i] = clip.entry;
                    self.records[i] = record;
                }
                None => {
                    index.insert(key, self.entries.len());
                    self.entries.push(clip.entry);
                    self.records.push(record);
                }
            }
        }
        self.reindex();
        if self.selection.is_empty() {
            self.confirm_delete = false;
        }
        self.restore_scroll(anchor);
    }

    fn remove_gone(&mut self, changes: &ClipChanges, now: Instant) {
        let mut gone: HashSet<String> = changes.gone.iter().map(|p| folder_key(p)).collect();
        if !changes.gone_under.is_empty() {
            let under: HashSet<String> = changes.gone_under.iter().map(|p| folder_key(p)).collect();
            let inside = |key: &str| Path::new(key).ancestors().skip(1).any(|a| a.to_str().is_some_and(|a| under.contains(a)));
            gone.extend(self.entries.iter().map(|e| folder_key(&e.path)).filter(|key| inside(key)));
        }
        if gone.is_empty() {
            return;
        }
        let paths: Vec<String> = self.entries.iter().filter(|e| gone.contains(&folder_key(&e.path))).map(|e| e.path.clone()).collect();
        for path in &paths {
            self.forget_assets(path);
            if self.hovered.as_deref() == Some(path.as_str()) {
                self.hovered = None;
            }
            if self.renaming.as_deref() == Some(path.as_str()) {
                self.renaming = None;
            }
            if self.anchor.as_deref() == Some(path.as_str()) {
                self.anchor = None;
            }
        }
        self.records.retain(|r| !gone.contains(&folder_key(&r.path)));
        self.entries.retain(|e| !gone.contains(&folder_key(&e.path)));
        self.recent.extend(gone.into_iter().map(|key| (key, (now, None))));
    }

    /// Lazy-asset bookkeeping for one path, so a new revision of it loads fresh.
    fn forget_assets(&mut self, path: &str) {
        self.thumb_queued.remove(path);
        self.thumb_failed.remove(path);
        self.duration_queued.remove(path);
        self.scrub_requested.remove(path);
        self.scrub.remove(path);
    }

    /// A listing may have started before the changes it is delivered after: they go on top.
    /// Clips it already has stay as listed (with their thumbnails).
    pub(super) fn replay_recent(&mut self) {
        if self.recent.is_empty() {
            return;
        }
        let listed: HashSet<String> = self.entries.iter().map(|e| folder_key(&e.path)).collect();
        self.entries.retain(|e| !matches!(self.recent.get(&folder_key(&e.path)), Some((_, None))));
        for (key, (_, entry)) in &self.recent {
            if let Some(entry) = entry.as_ref().filter(|_| !listed.contains(key)) {
                self.entries.push(entry.clone());
            }
        }
    }

    /// The replay log no longer applies (a fresh listing is coming, or the folders changed).
    pub fn forget_recent(&mut self) {
        self.recent.clear();
    }

    /// A clip with exactly this path is listed.
    pub fn lists(&self, path: &str) -> bool {
        self.by_path.contains_key(path)
    }

    /// Where the list is scrolled, as a clip; `None` at the very top (new clips stay in view).
    pub(super) fn scroll_anchor(&self) -> Option<ScrollAnchor> {
        let top = self.list.logical_scroll_top();
        if top.item_ix == 0 && f32::from(top.offset_in_item) <= 0.0 {
            return None;
        }
        let tiles = self.rows.iter().enumerate().skip(top.item_ix).find_map(|(ix, row)| match row {
            Row::Tiles(items) => items.first().map(|i| (ix, self.records[*i].path.clone())),
            Row::Header { .. } => None,
        });
        let (below_top, path) = tiles.map_or((0, None), |(ix, path)| (ix - top.item_ix, Some(path)));
        Some(ScrollAnchor { path, top: top.item_ix, below_top, offset: top.offset_in_item })
    }

    pub(super) fn restore_scroll(&self, anchor: Option<ScrollAnchor>) {
        let Some(anchor) = anchor else { return };
        let row = anchor.path.as_ref().and_then(|path| {
            self.rows.iter().position(|row| matches!(row, Row::Tiles(items) if items.iter().any(|i| self.records[*i].path == *path)))
        });
        let item_ix = row.map_or(anchor.top, |ix| ix.saturating_sub(anchor.below_top));
        self.list.scroll_to(ListOffset { item_ix, offset_in_item: anchor.offset });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        library_state::LibraryState,
        services::library::{MetaStore, watch::AddedClip},
    };

    fn entry(path: &str, bytes: u64) -> ClipEntry {
        let name = Path::new(path).file_name().unwrap().to_string_lossy().into_owned();
        ClipEntry { name, path: path.into(), bytes, mtime: 1_700_000_000, thumb: None, folder: None }
    }

    fn listing(entries: Vec<ClipEntry>) -> crate::worker::Listing {
        crate::worker::Listing { entries, sidecars: Default::default(), drive: None }
    }

    fn added(path: &str, bytes: u64) -> AddedClip {
        AddedClip { entry: entry(path, bytes), edited: false }
    }

    fn library(paths: &[&str]) -> LibraryState {
        let mut lib = LibraryState::new();
        lib.store = MetaStore::default();
        lib.set_entries(listing(paths.iter().map(|p| entry(p, 10)).collect()));
        lib
    }

    fn listed(lib: &LibraryState) -> Vec<String> {
        let mut paths: Vec<String> = lib.records.iter().map(|r| r.path.clone()).collect();
        paths.sort();
        paths
    }

    #[test]
    fn a_new_clip_becomes_one_tile_and_a_second_report_adds_nothing() {
        let mut lib = library(&[r"D:\c\a.mp4"]);
        lib.apply_changes(ClipChanges { added: vec![added(r"D:\c\b.mp4", 5)], ..Default::default() });
        assert_eq!(listed(&lib), [r"D:\c\a.mp4", r"D:\c\b.mp4"]);
        assert_eq!(lib.visible_count, 2);
        // Recall's own save: listed first, then reported by the watcher (other case on the wire).
        lib.apply_changes(ClipChanges { added: vec![added(r"D:\C\B.mp4", 5)], ..Default::default() });
        assert_eq!(lib.records.len(), 2, "no double tile");
    }

    #[test]
    fn an_unchanged_clip_keeps_its_thumbnail_and_a_changed_one_loses_it() {
        let mut lib = library(&[r"D:\c\a.mp4"]);
        lib.records[0].thumb = Some("t.jpg".into());
        lib.entries[0].thumb = Some("t.jpg".into());
        lib.apply_changes(ClipChanges { added: vec![added(r"D:\c\a.mp4", 10)], ..Default::default() });
        assert_eq!(lib.records[0].thumb.as_deref(), Some("t.jpg"));
        lib.thumb_failed.insert(r"D:\c\a.mp4".into());
        lib.apply_changes(ClipChanges { added: vec![added(r"D:\c\a.mp4", 11)], ..Default::default() });
        assert_eq!((lib.records.len(), lib.records[0].bytes, lib.records[0].thumb.is_none()), (1, 11, true));
        assert!(lib.thumb_failed.is_empty(), "the new revision may try again");
    }

    #[test]
    fn removed_clips_leave_selection_hover_and_the_delete_question() {
        let mut lib = library(&[r"D:\c\a.mp4", r"D:\c\b.mp4"]);
        lib.ask_delete(&[r"D:\c\a.mp4".to_owned()]);
        lib.hovered = Some(r"D:\c\a.mp4".into());
        lib.renaming = Some(r"D:\c\a.mp4".into());
        lib.apply_changes(ClipChanges { gone: vec![r"d:\C\A.mp4".into()], ..Default::default() });
        assert_eq!(listed(&lib), [r"D:\c\b.mp4"]);
        assert!(lib.selection.is_empty() && !lib.confirm_delete && lib.hovered.is_none() && lib.renaming.is_none());
        assert!(lib.lists(r"D:\c\b.mp4") && !lib.lists(r"D:\c\a.mp4"));
    }

    #[test]
    fn a_folder_that_left_takes_its_clips_with_it() {
        let mut lib = library(&[r"D:\c\game\a.mp4", r"D:\c\game\deep\b.mp4", r"D:\c\game2\c.mp4"]);
        lib.apply_changes(ClipChanges { gone_under: vec![r"D:\c\Game".into()], ..Default::default() });
        assert_eq!(listed(&lib), [r"D:\c\game2\c.mp4"]);
    }

    #[test]
    fn a_listing_that_started_before_a_change_does_not_undo_it() {
        let mut lib = library(&[r"D:\c\a.mp4", r"D:\c\old.mp4"]);
        lib.apply_changes(ClipChanges { added: vec![added(r"D:\c\new.mp4", 5)], gone: vec![r"D:\c\old.mp4".into()], ..Default::default() });
        lib.set_entries(listing(vec![entry(r"D:\c\a.mp4", 10), entry(r"D:\c\old.mp4", 10)]));
        assert_eq!(listed(&lib), [r"D:\c\a.mp4", r"D:\c\new.mp4"]);
        // A listing that already has the clip keeps its own entry; no double tile.
        lib.set_entries(listing(vec![entry(r"D:\c\a.mp4", 10), entry(r"D:\c\new.mp4", 5)]));
        assert_eq!(listed(&lib), [r"D:\c\a.mp4", r"D:\c\new.mp4"]);
        // After a resync the listing is the truth.
        lib.forget_recent();
        lib.set_entries(listing(vec![entry(r"D:\c\a.mp4", 10)]));
        assert_eq!(listed(&lib), [r"D:\c\a.mp4"]);
    }

    #[test]
    fn the_scroll_position_follows_the_clip_it_was_on() {
        let paths: Vec<String> = (0..40).map(|i| format!(r"D:\c\G__2026-10-{:02}__10-00-00-{i:03}.mp4", 1 + i % 28)).collect();
        let refs: Vec<&str> = paths.iter().map(String::as_str).collect();
        let mut lib = library(&refs);
        lib.list.scroll_to(ListOffset { item_ix: 12, offset_in_item: gpui::px(7.) });
        let before = lib.scroll_anchor().unwrap();
        let path = before.path.clone().unwrap();
        // A clip on a new day adds a header and a row above.
        lib.apply_changes(ClipChanges { added: vec![added(r"D:\c\G__2026-10-30__10-00-00.mp4", 5)], ..Default::default() });
        let top = lib.list.logical_scroll_top();
        assert_eq!(f32::from(top.offset_in_item), 7.0);
        assert!(matches!(&lib.rows[top.item_ix + before.below_top], Row::Tiles(items) if items.iter().any(|i| lib.records[*i].path == path)));
        assert_eq!(top.item_ix, 14);
    }
}
