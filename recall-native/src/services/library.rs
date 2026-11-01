//! Clip library index: game names, day groups, search/sort, per-clip metadata
//! (favourite, custom title, cached duration), Recycle Bin delete, hover-scrub frames.
use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};

use crate::backend::ClipEntry;

mod calendar;
pub mod folders;
mod mp4;
pub mod mp4_map;
mod names;
mod scrub;
mod settle;
mod store;
pub mod watch;
mod watch_win;
mod win;
#[cfg(test)]
mod tests;
#[cfg(test)]
mod tests_io;
#[cfg(test)]
mod tests_settle;
#[cfg(test)]
mod tests_watch;

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
    names::normalize(prefix)
}

/// Parses `GAME__YYYY-MM-DD__HH-MM-SS[-mmm][__hash].mp4` into (game prefix, local start).
pub fn parse_file_name(file_name: &str) -> Option<(String, LocalStamp)> {
    names::parse(file_name)
}

/// Converts Unix seconds to local wall-clock time.
pub fn local_from_unix(secs: u64) -> LocalStamp {
    win::local_from_unix(secs)
}

/// Builds records from backend entries plus stored metadata. Cheap; no file reads
/// except sidecar existence checks.
pub fn build_records(entries: &[ClipEntry], store: &MetaStore) -> Vec<ClipRecord> {
    records_with(entries, &sidecar_index(entries), store)
}

/// [`build_records`] with the sidecars already listed: touches no disk.
pub fn records_with(entries: &[ClipEntry], sidecars: &HashSet<String>, store: &MetaStore) -> Vec<ClipRecord> {
    entries
        .iter()
        .map(|entry| {
            let edited = sidecar_candidates(Path::new(&entry.path)).iter().any(|candidate| sidecars.contains(&sidecar_key(candidate)));
            build_record(entry, edited, store)
        })
        .collect()
}

/// One record, when the caller already knows whether the clip has an edit sidecar.
pub fn build_record(entry: &ClipEntry, edited: bool, store: &MetaStore) -> ClipRecord {
    let (game_key, game) = normalize_game(&game_prefix(entry));
    let started = parse_file_name(&entry.name).map_or_else(|| local_from_unix(entry.mtime), |(_, s)| s);
    let meta = store.meta(&entry.name);
    ClipRecord {
        path: entry.path.clone(),
        file_name: entry.name.clone(),
        bytes: entry.bytes,
        mtime: entry.mtime,
        thumb: entry.thumb.clone(),
        game_key,
        game,
        started,
        duration: store.duration(&entry.name, entry.bytes, entry.mtime),
        edited,
        favourite: meta.is_some_and(|m| m.favourite),
        title: meta.and_then(|m| m.title.clone()),
    }
}

/// Game for a clip: the dated file-name prefix, else (imported clips in a per-game folder,
/// e.g. ShadowPlay or Medal) the folder name, else the undated file stem.
fn game_prefix(entry: &ClipEntry) -> String {
    match parse_file_name(&entry.name) {
        Some((prefix, _)) if !prefix.is_empty() => prefix,
        Some(_) => entry.folder.clone().unwrap_or_default(),
        None => entry.folder.clone().unwrap_or_else(|| names::fallback_prefix(&entry.name).to_owned()),
    }
}

/// The editor writes `<file>.recall-edit.json` (backend `save_project`); `<stem>.recall-edit.json`
/// is accepted too.
pub(crate) fn sidecar_candidates(clip: &Path) -> Vec<PathBuf> {
    let Some(name) = clip.file_name().map(|n| n.to_string_lossy().into_owned()) else { return Vec::new() };
    let mut candidates = vec![clip.with_file_name(format!("{name}.recall-edit.json"))];
    if let Some(stem) = clip.file_stem().map(|s| s.to_string_lossy().into_owned()) {
        candidates.push(clip.with_file_name(format!("{stem}.recall-edit.json")));
    }
    candidates
}

fn sidecar_key(path: &Path) -> String {
    path.to_string_lossy().to_lowercase()
}

/// One directory listing per clip folder instead of two stats per clip. Reads the clip drive:
/// call it off the UI thread.
pub fn sidecar_index(entries: &[ClipEntry]) -> HashSet<String> {
    let folders: HashSet<PathBuf> = entries.iter().filter_map(|e| Path::new(&e.path).parent().map(Path::to_path_buf)).collect();
    let mut index = HashSet::new();
    for folder in folders {
        let Ok(listing) = std::fs::read_dir(&folder) else { continue };
        for item in listing.flatten() {
            let name = item.file_name().to_string_lossy().to_lowercase();
            if name.ends_with(".recall-edit.json") {
                index.insert(sidecar_key(&folder.join(item.file_name())));
            }
        }
    }
    index
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
    let query = filter.query.trim().to_lowercase();
    let mut indices: Vec<usize> = records
        .iter()
        .enumerate()
        .filter(|(_, r)| filter.game_key.as_ref().is_none_or(|k| &r.game_key == k))
        .filter(|(_, r)| !filter.favourites_only || r.favourite)
        .filter(|(_, r)| {
            query.is_empty()
                || r.display_title().to_lowercase().contains(&query)
                || r.game.to_lowercase().contains(&query)
                || r.file_name.to_lowercase().contains(&query)
        })
        .map(|(i, _)| i)
        .collect();
    let newest = |a: &usize, b: &usize| records[*b].started.cmp(&records[*a].started).then_with(|| records[*a].file_name.cmp(&records[*b].file_name));
    match order {
        SortOrder::Newest => indices.sort_by(newest),
        SortOrder::Oldest => indices.sort_by(|a, b| newest(b, a)),
        SortOrder::Longest => indices.sort_by(|a, b| records[*b].duration.unwrap_or(0.0).total_cmp(&records[*a].duration.unwrap_or(0.0)).then_with(|| newest(a, b))),
        SortOrder::Largest => indices.sort_by(|a, b| records[*b].bytes.cmp(&records[*a].bytes).then_with(|| newest(a, b))),
    }
    indices
}

/// Groups already-sorted indices by local day. `today` decides "Today"/"Yesterday".
/// For Longest/Largest sorts the caller passes `by_day = false` and gets one group.
pub fn group_by_day(records: &[ClipRecord], sorted: &[usize], today: LocalStamp, by_day: bool) -> Vec<DayGroup> {
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
            groups.push(DayGroup { label: calendar::day_label(date, today), date, items: vec![], total_secs: 0.0, durations_complete: true });
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
    if mb >= 1_000_000.0 {
        format!("{:.2} TB", mb / 1_000_000.0)
    } else if mb >= 1000.0 {
        format!("{:.2} GB", mb / 1000.0)
    } else if mb >= 100.0 {
        format!("{mb:.0} MB")
    } else {
        format!("{mb:.1} MB")
    }
}

/// Persistent per-clip metadata at `%APPDATA%\Recall\library.json` (atomic writes).
#[derive(Debug, Default)]
pub struct MetaStore {
    /// None = in-memory only (a `Default` store, or a file that could not be read safely).
    path: Option<PathBuf>,
    clips: BTreeMap<String, store::ClipMeta>,
    /// Unflushed duration-cache entries.
    dirty: bool,
    /// Why writes are refused (an unreadable existing file must not be overwritten).
    write_block: Option<String>,
}

/// Reads an MP4's duration from its `moov/mvhd` box without decoding.
pub fn mp4_duration(path: &Path) -> Option<f64> {
    mp4::duration(path)
}

/// Length of an MP4's video track (where its last frame ends), without decoding.
pub fn mp4_video_duration(path: &Path) -> Option<f64> {
    let mut file = std::fs::File::open(path).ok()?;
    mp4::video_duration_of(&mut file)
}

/// Where an MP4's last video frame starts (its track can run on, holding that frame).
pub fn mp4_last_frame(path: &Path) -> Option<f64> {
    let mut file = std::fs::File::open(path).ok()?;
    mp4::last_frame_of(&mut file)
}

/// Where a file's picture ends: (track end, start of the last frame). Reads the file's index,
/// so call it off the UI thread.
pub fn video_limits(path: &str, duration: f64) -> (Option<f64>, Option<f64>) {
    let file = crate::services::storage::plain_path(Path::new(path));
    // MP4/MOV list the frames; for others (MKV) keep a tenth of a second clear of the end.
    let end = mp4_video_duration(&file).or(Some(duration));
    let last = mp4_last_frame(&file).or_else(|| end.filter(|end| *end > 0.2).map(|end| end - 0.1));
    (end, last)
}

/// Moves clips (plus their recorder thumbnail and edit sidecar) to the Recycle Bin.
/// Returns how many clips moved.
pub fn delete_to_recycle_bin(clips: &[PathBuf]) -> Result<usize, String> {
    win::recycle(clips)
}

/// Eight low-resolution cached frames across the clip for hover-scrub.
/// Blocking (ffmpeg); call from a worker thread.
pub fn scrub_frames(path: &str) -> Result<Vec<String>, String> {
    scrub::frames(path)
}

/// All eight scrub frames when already cached on the SSD (stats only, no extraction).
pub fn cached_scrub_frames(path: &str) -> Option<Vec<String>> {
    scrub::cached_in(&crate::services::storage::cache_root(), path)
}

/// (free bytes, total bytes) of the volume holding `path`.
pub fn drive_space(path: &Path) -> Option<(u64, u64)> {
    win::drive_space(path)
}

/// Today's local date stamp.
pub fn today() -> LocalStamp {
    win::today()
}
