//! Whole-cache inventory and budget enforcement (eviction order in `evict`).
//!
//! Units: `video/<id>.mp4` · `editor/<engine key>/` (prepared audio stems) ·
//! `visuals/<id>#editor` (poster + filmstrip) · `visuals/<id>#scrub` · `thumbs/<id>.jpg`.
//! Everything else (working projects, settings, indexes, history, temp files) is counted
//! but never evicted. The clip open in the editor is kept whole.
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use super::evict::{self, Tier, Unit};

/// Serializes index read-modify-writes and enforcement passes.
pub(super) static INDEX_LOCK: Mutex<()> = Mutex::new(());
/// Units of the clip most recently opened in the editor.
static KEEP: Mutex<Vec<String>> = Mutex::new(Vec::new());

pub(super) fn now_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

fn ms(time: std::io::Result<SystemTime>) -> u64 {
    time.ok().and_then(|t| t.duration_since(UNIX_EPOCH).ok()).map_or(0, |d| d.as_millis() as u64)
}

/// `media.json`: editor-media last use and which clip owns each engine cache folder (the
/// engine keys its folders by input path, not by clip identity).
#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub(super) struct Media {
    pub used: BTreeMap<String, u64>,
    pub owners: BTreeMap<String, String>,
}

pub(super) fn read_media(root: &Path) -> Media {
    std::fs::read(root.join("media.json")).ok().and_then(|bytes| serde_json::from_slice(&bytes).ok()).unwrap_or_default()
}

fn write_media(root: &Path, media: &Media) {
    if let Ok(bytes) = serde_json::to_vec(media) {
        let _ = super::projects::write_atomic(&root.join("media.json"), &bytes);
    }
}

pub(super) struct Inventory {
    pub units: Vec<Unit>,
    pub projects: u64,
    /// Settings, indexes, history, temp and in-progress files.
    pub other: u64,
}

impl Inventory {
    pub fn total(&self) -> u64 {
        self.units.iter().map(|u| u.bytes).sum::<u64>() + self.projects + self.other
    }
    pub fn tier_bytes(&self, tier: Tier) -> u64 {
        self.units.iter().filter(|u| u.tier == tier).map(|u| u.bytes).sum()
    }
}

/// Clip identity a unit belongs to (`None` for an engine folder with no recorded owner).
pub(super) fn unit_clip<'a>(name: &'a str, media: &'a Media) -> Option<&'a str> {
    let (folder, rest) = name.split_once('/')?;
    match folder {
        "video" => rest.strip_suffix(".mp4"),
        "thumbs" => rest.strip_suffix(".jpg"),
        "visuals" => rest.split_once('#').map(|(id, _)| id),
        "editor" => media.owners.get(rest).map(String::as_str),
        _ => None,
    }
}

/// Bytes and newest modification (ms) under `dir`.
pub(super) fn dir_size(dir: &Path) -> (u64, u64) {
    let (mut bytes, mut newest) = (0u64, 0u64);
    let mut stack = vec![dir.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(listing) = std::fs::read_dir(&dir) else { continue };
        for entry in listing.flatten() {
            let Ok(metadata) = entry.metadata() else { continue };
            if metadata.is_dir() {
                stack.push(entry.path());
            } else {
                bytes += metadata.len();
                newest = newest.max(ms(metadata.modified()));
            }
        }
    }
    (bytes, newest)
}

fn entries(dir: &Path) -> Vec<(String, std::fs::Metadata, PathBuf)> {
    std::fs::read_dir(dir)
        .map(|listing| listing.flatten().filter_map(|e| Some((e.file_name().to_string_lossy().into_owned(), e.metadata().ok()?, e.path()))).collect())
        .unwrap_or_default()
}

/// Everything under `root`, each unit scored by `score(clip id)`.
pub(super) fn inventory(root: &Path, score: &dyn Fn(&str) -> f64) -> Inventory {
    let video_index = evict::read_index(&root.join("video").join("lru.json"));
    let media = read_media(root);
    let mut units = Vec::new();
    let mut other = 0u64;
    let mut push = |tier: Tier, name: String, bytes: u64, last_used: u64| {
        let clip_score = unit_clip(&name, &media).map_or(0.0, score);
        units.push(Unit { tier, name, bytes, last_used, score: clip_score });
    };
    for (name, metadata, _) in entries(&root.join("video")) {
        if metadata.is_file() && name.ends_with(".mp4") {
            let last_used = video_index.get(&name).copied().unwrap_or_else(|| ms(metadata.created()));
            push(Tier::Video, format!("video/{name}"), metadata.len(), last_used);
        } else if metadata.is_file() {
            other += metadata.len();
        }
    }
    for (name, metadata, path) in entries(&root.join("editor")) {
        if metadata.is_dir() {
            let key = format!("editor/{name}");
            let (bytes, newest) = dir_size(&path);
            let last_used = media.used.get(&key).copied().unwrap_or(0).max(newest);
            push(Tier::EditorMedia, key, bytes, last_used);
        } else {
            other += metadata.len();
        }
    }
    for (id, metadata, path) in entries(&root.join("visuals")) {
        if !metadata.is_dir() {
            other += metadata.len();
            continue;
        }
        let (mut editor, mut scrub) = ((0u64, 0u64), (0u64, 0u64));
        for (name, file, _) in entries(&path) {
            let slot = if name.starts_with("scrub-") { &mut scrub } else { &mut editor };
            slot.0 += file.len();
            slot.1 = slot.1.max(ms(file.modified()));
        }
        if editor.0 > 0 {
            let last_used = media.used.get(&format!("visuals/{id}")).copied().unwrap_or(0).max(editor.1);
            push(Tier::EditorMedia, format!("visuals/{id}#editor"), editor.0, last_used);
        }
        if scrub.0 > 0 {
            push(Tier::Scrub, format!("visuals/{id}#scrub"), scrub.0, scrub.1);
        }
    }
    for (name, metadata, _) in entries(&root.join("thumbs")) {
        if metadata.is_file() {
            push(Tier::Thumb, format!("thumbs/{name}"), metadata.len(), ms(metadata.modified()));
        }
    }
    let projects = dir_size(&root.join("projects")).0;
    other += dir_size(&root.join("tmp")).0;
    for file in ["storage.json", "media.json", "interactions.json"] {
        other += root.join(file).metadata().map_or(0, |m| m.len());
    }
    Inventory { units, projects, other }
}

/// Deletes one unit; returns the bytes actually freed (files still open are skipped).
pub(super) fn delete(root: &Path, unit: &Unit) -> u64 {
    let (folder, rest) = unit.name.split_once('/').unwrap_or(("", &unit.name));
    let removed = |path: &Path| -> u64 {
        let bytes = path.metadata().map_or(0, |m| m.len());
        if std::fs::remove_file(path).is_ok() { bytes } else { 0 }
    };
    match (folder, rest.split_once('#')) {
        ("video" | "thumbs", None) => removed(&root.join(folder).join(rest)),
        ("editor", None) => {
            let dir = root.join("editor").join(rest);
            let before = dir_size(&dir).0;
            let _ = std::fs::remove_dir_all(&dir);
            before.saturating_sub(dir_size(&dir).0)
        }
        ("visuals", Some((id, part))) => {
            let dir = root.join("visuals").join(id);
            let freed = entries(&dir)
                .iter()
                .filter(|(name, _, _)| name.starts_with("scrub-") == (part == "scrub"))
                .map(|(_, _, path)| removed(path))
                .sum();
            let _ = std::fs::remove_dir(&dir); // only succeeds once empty
            freed
        }
        _ => 0,
    }
}

/// Evicts down to `budget` (video ≤ half of it), counting `incoming_video` bytes about to
/// join the video tier. Lowest-score clips first (`evict::plan`). Returns bytes freed.
pub(super) fn enforce(root: &Path, budget: u64, incoming_video: u64) -> u64 {
    let _guard = INDEX_LOCK.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    let inventory = inventory(root, &super::prefetch::score_of);
    let keep = kept();
    let evicted = evict::plan(&inventory.units, inventory.projects + inventory.other, budget, budget / 2, incoming_video, &keep);
    let mut freed = 0;
    for name in &evicted {
        if let Some(unit) = inventory.units.iter().find(|u| &u.name == name) {
            freed += delete(root, unit);
        }
    }
    if !evicted.is_empty() {
        let mut index = evict::read_index(&root.join("video").join("lru.json"));
        index.retain(|key, _| !evicted.contains(&format!("video/{key}")));
        let _ = evict::write_index(&root.join("video").join("lru.json"), &index);
        let mut media = read_media(root);
        media.used.retain(|key, _| !evicted.iter().any(|e| e == key || e.strip_suffix("#editor") == Some(key)));
        media.owners.retain(|key, _| !evicted.contains(&format!("editor/{key}")));
        write_media(root, &media);
        super::prefetch::invalidate_usage();
    }
    freed
}

/// Records which clip owns the engine cache folders holding its audio stems; with `opened`,
/// marks the clip as the one in the editor (fresh, and kept whole by every eviction).
pub(super) fn note_editor_media(root: &Path, id: &str, editor_dirs: &[PathBuf], opened: bool) {
    let _guard = INDEX_LOCK.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    let editor_root = root.join("editor");
    let mut keep = vec![format!("video/{id}.mp4"), format!("visuals/{id}#editor"), format!("visuals/{id}#scrub"), format!("thumbs/{id}.jpg")];
    let mut media = read_media(root);
    let now = now_ms();
    if opened {
        media.used.insert(format!("visuals/{id}"), now);
    }
    for dir in editor_dirs {
        // The engine reports canonical (`\\?\`) paths; compare plain and case-insensitively.
        let dir = super::plain_path(dir);
        let inside = dir.parent().is_some_and(|parent| same_dir(parent, &editor_root));
        if let (true, Some(name)) = (inside, dir.file_name()) {
            let name = name.to_string_lossy().into_owned();
            media.owners.insert(name.clone(), id.to_owned());
            if opened {
                media.used.insert(format!("editor/{name}"), now);
            }
            keep.push(format!("editor/{name}"));
        }
    }
    write_media(root, &media);
    if opened {
        if let Ok(mut current) = KEEP.lock() {
            *current = keep;
        }
    }
}

fn same_dir(a: &Path, b: &Path) -> bool {
    let text = |p: &Path| super::plain_path(p).to_string_lossy().trim_end_matches('\\').to_lowercase();
    text(a) == text(b)
}

pub(super) fn kept() -> Vec<String> {
    KEEP.lock().map(|k| k.clone()).unwrap_or_default()
}
