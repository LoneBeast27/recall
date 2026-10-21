//! Clip library index: game names, day groups, search/sort, per-clip metadata
//! (favourite, custom title, cached duration), Recycle Bin delete, hover-scrub frames.
use std::path::{Path, PathBuf};

use crate::backend::ClipEntry;

/// Local wall-clock time of a clip start (from the file name, else file mtime).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct LocalStamp {
    pub year: i32,
    pub month: u32,
    pub day: u32,
    pub hour: u32,
    pub minute: u32,
    pub second: u32,
}

impl LocalStamp {
    pub fn date_key(&self) -> (i32, u32, u32) {
        (self.year, self.month, self.day)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct ClipRecord {
    pub path: String,
    pub file_name: String,
    pub bytes: u64,
    /// Unix seconds.
    pub mtime: u64,
    pub thumb: Option<String>,
    /// Merge key: lowercase alphanumerics of the cleaned prefix ("rocketleague").
    pub game_key: String,
    /// Display name ("Rocket League", "VALORANT", "Desktop").
    pub game: String,
    pub started: LocalStamp,
    pub duration: Option<f64>,
    /// A `.recall-edit.json` sidecar exists.
    pub edited: bool,
    pub favourite: bool,
    pub title: Option<String>,
}

impl ClipRecord {
    /// Custom title if set, else the game name.
    pub fn display_title(&self) -> &str {
        self.title.as_deref().unwrap_or(&self.game)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct GameCount {
    pub key: String,
    pub name: String,
    pub count: usize,
}

#[derive(Debug, Clone, PartialEq)]
pub struct DayGroup {
    /// "Today", "Yesterday", "Saturday, 17 October", "Saturday, 17 October 2025".
    pub label: String,
    pub date: (i32, u32, u32),
    /// Indices into the filtered record slice, in display order.
    pub items: Vec<usize>,
    pub total_secs: f64,
    /// True when every clip in the group has a known duration.
    pub durations_complete: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SortOrder {
    #[default]
    Newest,
    Oldest,
    Longest,
    Largest,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Filter {
    pub query: String,
    pub game_key: Option<String>,
    pub favourites_only: bool,
}

/// Cleans a recorder file-name prefix into (merge key, display name).
/// "Rocket-League®" → ("rocketleague", "Rocket League"); "DESKTOPCAPTURE" → ("desktop", "Desktop").
pub fn normalize_game(prefix: &str) -> (String, String) {
    let display = prefix.replace(['-', '_'], " ");
    let key = display.chars().filter(|c| c.is_alphanumeric()).flat_map(char::to_lowercase).collect();
    (key, display)
}

/// Parses `GAME__YYYY-MM-DD__HH-MM-SS[-mmm][__hash].mp4` into (game prefix, local start).
pub fn parse_file_name(_file_name: &str) -> Option<(String, LocalStamp)> {
    None
}

/// Converts Unix seconds to local wall-clock time.
pub fn local_from_unix(_secs: u64) -> LocalStamp {
    LocalStamp::default()
}

/// Builds records from backend entries plus stored metadata. Cheap; no file reads
/// except sidecar existence checks.
pub fn build_records(entries: &[ClipEntry], _store: &MetaStore) -> Vec<ClipRecord> {
    entries
        .iter()
        .map(|entry| {
            let (game_key, game) = normalize_game(entry.name.split("__").next().unwrap_or(&entry.name));
            ClipRecord {
                path: entry.path.clone(),
                file_name: entry.name.clone(),
                bytes: entry.bytes,
                mtime: entry.mtime,
                thumb: entry.thumb.clone(),
                game_key,
                game,
                started: local_from_unix(entry.mtime),
                duration: None,
                edited: false,
                favourite: false,
                title: None,
            }
        })
        .collect()
}

/// Games with counts, most clips first; variants merged by key.
pub fn game_counts(records: &[ClipRecord]) -> Vec<GameCount> {
    let mut counts: Vec<GameCount> = Vec::new();
    for record in records {
        match counts.iter_mut().find(|g| g.key == record.game_key) {
            Some(game) => game.count += 1,
            None => counts.push(GameCount { key: record.game_key.clone(), name: record.game.clone(), count: 1 }),
        }
    }
    counts.sort_by(|a, b| b.count.cmp(&a.count).then_with(|| a.name.cmp(&b.name)));
    counts
}

/// Applies the filter and sort; returns indices into `records`.
pub fn filter_sort(records: &[ClipRecord], filter: &Filter, order: SortOrder) -> Vec<usize> {
    let query = filter.query.to_lowercase();
    let mut indices: Vec<usize> = records
        .iter()
        .enumerate()
        .filter(|(_, r)| filter.game_key.as_ref().is_none_or(|k| &r.game_key == k))
        .filter(|(_, r)| !filter.favourites_only || r.favourite)
        .filter(|(_, r)| query.is_empty() || r.display_title().to_lowercase().contains(&query) || r.game.to_lowercase().contains(&query))
        .map(|(i, _)| i)
        .collect();
    match order {
        SortOrder::Newest => indices.sort_by(|a, b| records[*b].started.cmp(&records[*a].started)),
        SortOrder::Oldest => indices.sort_by(|a, b| records[*a].started.cmp(&records[*b].started)),
        SortOrder::Longest => indices.sort_by(|a, b| records[*b].duration.unwrap_or(0.0).total_cmp(&records[*a].duration.unwrap_or(0.0))),
        SortOrder::Largest => indices.sort_by(|a, b| records[*b].bytes.cmp(&records[*a].bytes)),
    }
    indices
}

/// Groups already-sorted indices by local day. `today` decides "Today"/"Yesterday".
/// For Longest/Largest sorts the caller passes `by_day = false` and gets one group.
pub fn group_by_day(records: &[ClipRecord], sorted: &[usize], _today: LocalStamp, by_day: bool) -> Vec<DayGroup> {
    if !by_day {
        return vec![DayGroup {
            label: "All clips".into(),
            date: (0, 0, 0),
            items: sorted.to_vec(),
            total_secs: sorted.iter().filter_map(|i| records[*i].duration).sum(),
            durations_complete: sorted.iter().all(|i| records[*i].duration.is_some()),
        }];
    }
    let mut groups: Vec<DayGroup> = Vec::new();
    for &index in sorted {
        let date = records[index].started.date_key();
        if groups.last().is_none_or(|g| g.date != date) {
            groups.push(DayGroup { label: format!("{}-{:02}-{:02}", date.0, date.1, date.2), date, items: vec![], total_secs: 0.0, durations_complete: true });
        }
        let group = groups.last_mut().expect("group pushed");
        group.items.push(index);
        match records[index].duration {
            Some(d) => group.total_secs += d,
            None => group.durations_complete = false,
        }
    }
    groups
}

/// "1:59", "12:04", "1:02:03".
pub fn format_duration(secs: f64) -> String {
    let total = secs.max(0.0).round() as u64;
    let (h, m, s) = (total / 3600, (total % 3600) / 60, total % 60);
    if h > 0 { format!("{h}:{m:02}:{s:02}") } else { format!("{m}:{s:02}") }
}

/// "6.4 MB", "364 MB", "1.87 GB".
pub fn format_bytes(bytes: u64) -> String {
    let mb = bytes as f64 / 1_000_000.0;
    if mb >= 1000.0 { format!("{:.2} GB", mb / 1000.0) } else if mb >= 100.0 { format!("{mb:.0} MB") } else { format!("{mb:.1} MB") }
}

/// Persistent per-clip metadata at `%APPDATA%\Recall\library.json` (atomic writes).
#[derive(Debug, Default)]
pub struct MetaStore {
    _private: (),
}

impl MetaStore {
    pub fn load() -> Self {
        Self::default()
    }
    pub fn set_favourite(&mut self, _file_name: &str, _favourite: bool) -> Result<(), String> {
        Err("metadata store not implemented".into())
    }
    /// `None` or empty clears the custom title.
    pub fn set_title(&mut self, _file_name: &str, _title: Option<&str>) -> Result<(), String> {
        Err("metadata store not implemented".into())
    }
    /// Cached duration keyed by (file name, bytes, mtime).
    pub fn duration(&self, _file_name: &str, _bytes: u64, _mtime: u64) -> Option<f64> {
        None
    }
    pub fn set_duration(&mut self, _file_name: &str, _bytes: u64, _mtime: u64, _secs: f64) {}
    /// Persists pending duration-cache entries (debounced by the caller).
    pub fn flush(&mut self) -> Result<(), String> {
        Ok(())
    }
}

/// Reads an MP4's duration from its `moov/mvhd` box without decoding.
pub fn mp4_duration(_path: &Path) -> Option<f64> {
    None
}

/// Moves clips (plus their recorder thumbnail and edit sidecar) to the Recycle Bin.
/// Returns how many clips moved.
pub fn delete_to_recycle_bin(_clips: &[PathBuf]) -> Result<usize, String> {
    Err("Recycle Bin delete not implemented".into())
}

/// Eight low-resolution cached frames across the clip for hover-scrub.
/// Blocking (ffmpeg); call from a worker thread.
pub fn scrub_frames(_path: &str) -> Result<Vec<String>, String> {
    Err("scrub frames not implemented".into())
}

/// (free bytes, total bytes) of the volume holding `path`.
pub fn drive_space(_path: &Path) -> Option<(u64, u64)> {
    None
}

/// Today's local date stamp.
pub fn today() -> LocalStamp {
    LocalStamp::default()
}
