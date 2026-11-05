//! Library view model: records, filter/sort, day groups flattened into list rows,
//! selection, rename/delete flow, and lazy thumbnails / durations / scrub frames.
use std::{
    cell::Cell,
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
    rc::Rc,
    time::{Duration, Instant},
};

use gpui::{ListAlignment, ListState, px};

use crate::{
    backend::ClipEntry,
    services::library::{self, ClipRecord, DayGroup, Filter, GameCount, LocalStamp, MetaStore, SortOrder},
    worker::{AssetJob, Listing, Workers},
};

#[path = "library_changes.rs"]
mod changes;
#[path = "library_played.rs"]
mod played;
#[path = "library_select.rs"]
mod select;

pub const TILE_MIN_WIDTH: f32 = 220.0;
pub const SCRUB_FRAMES: usize = 8;
const VISIBLE_ROWS_AHEAD: usize = 10;
const META_FLUSH_AFTER: Duration = Duration::from_secs(3);

#[derive(Debug, Clone, PartialEq)]
pub enum Row {
    /// Group index; `first` adds less top padding.
    Header { group: usize, first: bool },
    Tiles(Vec<usize>),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    All,
    Favourites,
    Game,
}

/// How a title rename ends.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RenameEnd {
    /// Enter or Tab: the text is the new title; an empty field goes back to the game's name.
    Save,
    /// Escape: the title stays.
    Cancel,
    /// Click, page change, the tile leaving the screen. The typing was meant to count, so it
    /// saves; an empty field means nothing was meant, so the title stays.
    Away,
}

pub struct LibraryState {
    entries: Vec<ClipEntry>,
    pub records: Vec<ClipRecord>,
    by_path: HashMap<String, usize>,
    pub store: MetaStore,
    pub filter: Filter,
    pub sort: SortOrder,
    pub groups: Vec<DayGroup>,
    pub rows: Vec<Row>,
    pub visible_count: usize,
    pub columns: usize,
    pub list: ListState,
    pub games: Vec<GameCount>,
    pub favourites: usize,
    pub show_all_games: bool,
    pub selection: HashSet<String>,
    anchor: Option<String>,
    pub hovered: Option<String>,
    pub hover_frac: f32,
    pub hover_span: Rc<Cell<Option<(f32, f32)>>>,
    pub scrub: HashMap<String, Vec<String>>,
    scrub_requested: HashSet<String>,
    thumb_queued: HashSet<String>,
    thumb_failed: HashSet<String>,
    /// Last check that the visible tiles' thumbnail files still exist (once a second).
    thumb_check_at: Option<std::time::Instant>,
    duration_queued: HashSet<String>,
    pub loading: bool,
    pub error: Option<String>,
    pub notice: Option<String>,
    pub renaming: Option<String>,
    pub confirm_delete: bool,
    /// Clips on their way to the Recycle Bin (a hard drive can take a while).
    pub deleting: usize,
    pub content_width: Rc<Cell<f32>>,
    pub drive: Option<(u64, u64)>,
    pub today: LocalStamp,
    meta_dirty_since: Option<Instant>,
    deepest_scroll_days: u32,
    /// Watcher changes a listing in flight may predate.
    recent: changes::Recent,
    /// Path of the clip opened most recently.
    played: Option<String>,
    /// The game whose clip heads were last asked for (`library_probe`).
    probed_game: Option<String>,
}

impl LibraryState {
    pub fn new() -> Self {
        Self {
            entries: vec![],
            records: vec![],
            by_path: HashMap::new(),
            store: MetaStore::load(),
            filter: Filter::default(),
            sort: SortOrder::Newest,
            groups: vec![],
            rows: vec![],
            visible_count: 0,
            columns: 4,
            list: ListState::new(0, ListAlignment::Top, px(800.)),
            games: vec![],
            favourites: 0,
            show_all_games: false,
            selection: HashSet::new(),
            anchor: None,
            hovered: None,
            hover_frac: 0.0,
            hover_span: Rc::new(Cell::new(None)),
            scrub: HashMap::new(),
            scrub_requested: HashSet::new(),
            thumb_queued: HashSet::new(),
            thumb_failed: HashSet::new(),
            thumb_check_at: None,
            duration_queued: HashSet::new(),
            loading: true,
            error: None,
            notice: None,
            renaming: None,
            confirm_delete: false,
            deleting: 0,
            content_width: Rc::new(Cell::new(0.0)),
            drive: None,
            today: library::today(),
            meta_dirty_since: None,
            deepest_scroll_days: 0,
            recent: Default::default(),
            played: None,
            probed_game: None,
        }
    }

    pub fn scope(&self) -> Scope {
        if self.filter.game_key.is_some() {
            Scope::Game
        } else if self.filter.favourites_only {
            Scope::Favourites
        } else {
            Scope::All
        }
    }

    pub fn set_entries(&mut self, listing: Listing) {
        let anchor = self.scroll_anchor();
        self.entries = listing.entries;
        self.replay_recent();
        self.loading = false;
        self.today = library::today();
        // The meter shows the recordings drive, not whichever drive the newest import is on.
        self.drive = listing.drive;
        self.records = library::records_with(&self.entries, &listing.sidecars, &self.store);
        self.reindex();
        self.restore_scroll(anchor);
    }

    /// Everything derived from `records`. Touches no disk.
    fn reindex(&mut self) {
        self.by_path = self.records.iter().enumerate().map(|(i, r)| (r.path.clone(), i)).collect();
        self.games = library::game_counts(&self.records);
        self.favourites = self.records.iter().filter(|r| r.favourite).count();
        self.played = played::latest(&self.records);
        self.selection.retain(|path| self.by_path.contains_key(path));
        self.refilter();
    }

    /// By exact path, else by file name (the editor may hold a canonical `\\?\` path).
    pub fn record(&self, path: &str) -> Option<&ClipRecord> {
        if let Some(record) = self.by_path.get(path).and_then(|i| self.records.get(*i)) {
            return Some(record);
        }
        let name = Path::new(path).file_name()?.to_string_lossy();
        self.records.iter().find(|r| r.file_name == name)
    }

    pub fn refilter(&mut self) {
        let sorted = library::filter_sort(&self.records, &self.filter, self.sort);
        crate::library_probe::follow(&mut self.probed_game, &self.filter, &self.records);
        self.visible_count = sorted.len();
        let by_day = matches!(self.sort, SortOrder::Newest | SortOrder::Oldest);
        self.groups = library::group_by_day(&self.records, &sorted, self.today, by_day);
        self.rebuild_rows();
        // Clips out of view leave the selection: its count and its actions cover what is shown.
        if !self.selection.is_empty() {
            let shown: HashSet<usize> = self.groups.iter().flat_map(|g| g.items.iter().copied()).collect();
            let by_path = &self.by_path;
            self.selection.retain(|path| by_path.get(path).is_some_and(|i| shown.contains(i)));
            self.confirm_delete &= !self.selection.is_empty();
        }
    }

    /// Recomputes the column count from the measured content width.
    pub fn fit_columns(&mut self) -> bool {
        let width = self.content_width.get();
        if width <= 0.0 {
            return false;
        }
        let columns = (((width + 16.0) / (TILE_MIN_WIDTH + 16.0)).floor() as usize).clamp(2, 8);
        if columns != self.columns {
            self.columns = columns;
            self.rebuild_rows();
            return true;
        }
        false
    }

    fn rebuild_rows(&mut self) {
        let mut rows = Vec::new();
        for (index, group) in self.groups.iter().enumerate() {
            rows.push(Row::Header { group: index, first: index == 0 });
            for chunk in group.items.chunks(self.columns.max(1)) {
                rows.push(Row::Tiles(chunk.to_vec()));
            }
        }
        if rows != self.rows {
            self.rows = rows;
            self.list.reset(self.rows.len());
        }
    }

    // ---- lazy assets ----------------------------------------------------

    /// Queues thumbnails and durations for rows near the scroll position.
    pub fn queue_visible(&mut self, workers: &Workers) {
        let top = self.list.logical_scroll_top().item_ix;
        let end = (top + VISIBLE_ROWS_AHEAD).min(self.rows.len());
        let mut wanted = Vec::new();
        for row in self.rows.get(top.saturating_sub(2)..end).unwrap_or_default() {
            if let Row::Tiles(items) = row {
                wanted.extend(items.iter().copied());
            }
        }
        // A thumbnail file can vanish under a running app (a cache clear, eviction, a person
        // emptying the folder): a tile that points at a missing file asks for it again.
        let check = self.thumb_check_at.is_none_or(|at| at.elapsed() >= std::time::Duration::from_secs(1));
        if check {
            self.thumb_check_at = Some(std::time::Instant::now());
            for &index in &wanted {
                let missing = self.records[index].thumb.as_deref().is_some_and(|thumb| !Path::new(thumb).is_file());
                if missing {
                    let path = self.records[index].path.clone();
                    self.records[index].thumb = None;
                    if let Some(entry) = self.entries.iter_mut().find(|e| e.path == path) {
                        entry.thumb = None;
                    }
                    self.thumb_failed.remove(&path);
                }
            }
        }
        for index in wanted {
            let record = &self.records[index];
            if record.thumb.is_none() && !self.thumb_queued.contains(&record.path) && !self.thumb_failed.contains(&record.path) {
                self.thumb_queued.insert(record.path.clone());
                workers.asset(AssetJob::Thumb(record.path.clone()));
            }
            if record.duration.is_none() && self.duration_queued.insert(record.path.clone()) {
                workers.asset(AssetJob::Duration(record.path.clone()));
            }
        }
    }

    pub fn on_thumb(&mut self, path: String, result: Result<String, String>) -> bool {
        self.thumb_queued.remove(&path);
        match (result, self.by_path.get(&path).copied()) {
            (Ok(thumb), Some(index)) => {
                self.records[index].thumb = Some(thumb.clone());
                if let Some(entry) = self.entries.iter_mut().find(|e| e.path == path) {
                    entry.thumb = Some(thumb);
                }
                true
            }
            (Err(_), _) => {
                self.thumb_failed.insert(path);
                false
            }
            _ => false,
        }
    }

    pub fn on_duration(&mut self, path: String, secs: Option<f64>) -> bool {
        let (Some(secs), Some(index)) = (secs, self.by_path.get(&path).copied()) else { return false };
        let record = &mut self.records[index];
        record.duration = Some(secs);
        self.store.set_duration(&record.file_name, record.bytes, record.mtime, secs);
        self.meta_dirty_since.get_or_insert_with(Instant::now);
        for group in &mut self.groups {
            if group.items.contains(&index) {
                group.total_secs = group.items.iter().filter_map(|i| self.records[*i].duration).sum();
                group.durations_complete = group.items.iter().all(|i| self.records[*i].duration.is_some());
            }
        }
        true
    }

    pub fn flush_meta_if_due(&mut self) {
        if self.meta_dirty_since.is_some_and(|t| t.elapsed() >= META_FLUSH_AFTER) {
            self.meta_dirty_since = None;
            if let Err(error) = self.store.flush() {
                self.notice = Some(format!("Couldn't save library data: {error}"));
            }
        }
    }

    pub fn set_hover(&mut self, path: Option<String>, workers: &Workers) {
        if self.hovered == path {
            return;
        }
        self.hovered = path.clone();
        self.hover_frac = 0.0;
        if let Some(path) = path {
            interaction(crate::services::storage::Interaction::Hovered(path.clone()));
            if !self.scrub.contains_key(&path) && self.scrub_requested.insert(path.clone()) {
                workers.scrub(path);
            }
        }
    }

    pub fn on_scrub(&mut self, path: String, result: Result<Vec<String>, String>) -> bool {
        match result {
            Ok(frames) if !frames.is_empty() => {
                self.scrub.insert(path.clone(), frames);
                self.hovered.as_deref() == Some(path.as_str())
            }
            _ => false,
        }
    }

    /// The thumbnail is on its way from the drive (a failed one stays a plain placeholder).
    pub fn thumb_waiting(&self, path: &str) -> bool {
        !self.thumb_failed.contains(path)
    }

    /// Hover frames were asked for and are not here yet.
    pub fn scrub_waiting(&self, path: &str) -> bool {
        self.hovered.as_deref() == Some(path) && !self.scrub.contains_key(path)
    }

    /// Frame to show for the hovered tile, if scrub frames are ready.
    pub fn scrub_frame(&self, path: &str) -> Option<&str> {
        if self.hovered.as_deref() != Some(path) {
            return None;
        }
        let frames = self.scrub.get(path)?;
        let index = ((self.hover_frac * frames.len() as f32) as usize).min(frames.len() - 1);
        frames.get(index).map(String::as_str)
    }

    // ---- selection & metadata -------------------------------------------

    fn display_order(&self) -> Vec<String> {
        self.groups.iter().flat_map(|g| g.items.iter()).map(|i| self.records[*i].path.clone()).collect()
    }

    pub fn clear_selection(&mut self) {
        self.selection.clear();
        self.anchor = None;
        self.confirm_delete = false;
    }

    pub fn toggle_favourite(&mut self, paths: &[String]) {
        let make = paths.iter().any(|p| self.record(p).is_some_and(|r| !r.favourite));
        for path in paths {
            let Some(index) = self.by_path.get(path).copied() else { continue };
            let name = self.records[index].file_name.clone();
            match self.store.set_favourite(&name, make) {
                Ok(()) => {
                    self.records[index].favourite = make;
                    if make {
                        interaction(crate::services::storage::Interaction::Favourited(path.clone()));
                    }
                }
                Err(error) => self.notice = Some(format!("Couldn't save favourite: {error}")),
            }
        }
        self.favourites = self.records.iter().filter(|r| r.favourite).count();
        if self.filter.favourites_only {
            self.refilter();
        }
    }

    /// Ends an open rename; nothing happens when none is open.
    pub fn finish_rename(&mut self, text: &str, how: RenameEnd) {
        let Some(path) = self.renaming.take() else { return };
        let empty = text.trim().is_empty();
        if how == RenameEnd::Save || (how == RenameEnd::Away && !empty) {
            self.rename(&path, text);
        }
    }

    pub fn rename(&mut self, path: &str, title: &str) {
        let Some(index) = self.by_path.get(path).copied() else { return };
        let name = self.records[index].file_name.clone();
        let title = title.trim();
        let value = (!title.is_empty()).then_some(title);
        // Unchanged text must not pin today's game name as a custom title.
        if value == Some(self.records[index].display_title()) || (value.is_none() && self.records[index].title.is_none()) {
            return;
        }
        match self.store.set_title(&name, value) {
            Ok(()) => self.records[index].title = value.map(str::to_owned),
            Err(error) => self.notice = Some(format!("Couldn't rename: {error}")),
        }
    }

    /// Clips a tile menu acts on: the whole selection (in display order) when `path` is part
    /// of it, else just `path`.
    pub fn menu_targets(&self, path: &str) -> Vec<String> {
        menu_targets(&self.selection, self.display_order(), path)
    }

    /// Selects exactly `paths` and asks the inline "Move to Recycle Bin?" question.
    pub fn ask_delete(&mut self, paths: &[String]) {
        self.selection = paths.iter().cloned().collect();
        self.anchor = paths.first().cloned();
        self.confirm_delete = !self.selection.is_empty();
    }

    pub fn selected_paths(&self) -> Vec<PathBuf> {
        self.display_order().into_iter().filter(|p| self.selection.contains(p)).map(PathBuf::from).collect()
    }

    /// Removes deleted clips from the model without a full rescan.
    pub fn forget(&mut self, paths: &[PathBuf]) {
        let gone: HashSet<String> = paths.iter().map(|p| p.to_string_lossy().into_owned()).collect();
        self.entries.retain(|e| !gone.contains(&e.path));
        self.records.retain(|r| !gone.contains(&r.path));
        self.clear_selection();
        self.reindex();
    }
}

fn menu_targets(selection: &HashSet<String>, display_order: Vec<String>, path: &str) -> Vec<String> {
    if selection.contains(path) {
        display_order.into_iter().filter(|p| selection.contains(p)).collect()
    } else {
        vec![path.to_owned()]
    }
}

#[cfg(test)]
#[path = "library_state_tests.rs"]
mod rename_tests;

fn interaction(event: crate::services::storage::Interaction) {
    crate::services::storage::record_interaction(event);
}

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
