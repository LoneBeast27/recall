//! What the SSD cache holds, for the sidebar's SSD row: cache used against the one budget,
//! split into previews, kept copies and heads, plus exports that sit on the cache drive.
//!
//! [`ssd_usage`] walks the cache folder (thousands of small files): call it from a worker
//! thread. [`generation`] moves on every cache change that already marks the Settings totals
//! stale, so the UI can ask again only when something changed.
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};

use super::budget;
use super::evict::Tier;

static GENERATION: AtomicU64 = AtomicU64::new(0);

/// Called by `prefetch::invalidate_usage`: the cache changed.
pub(super) fn bump() {
    GENERATION.fetch_add(1, Ordering::Relaxed);
}

/// Changes whenever the cache changed (eviction, clear, budget, sweeps).
pub fn generation() -> u64 {
    GENERATION.load(Ordering::Relaxed)
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SsdUsage {
    /// Thumbnails, hover frames, editor media (poster, filmstrip, audio stems), preview copies.
    pub previews: u64,
    /// SSD copies of clips: the video tier (whole and light copies) and the recorder's kept copies.
    pub kept: u64,
    /// Clip heads from the filter probe.
    pub heads: u64,
    /// Working edit projects, settings and indexes, and the pre-encoded export waiting in the
    /// cache (held against the budget: `pre_encode_room`).
    pub other: u64,
    /// The one cache budget.
    pub budget: u64,
    /// Finished exports on the cache drive: the `Edited` folder when it is on the same volume.
    /// Not held against the budget.
    pub exports: u64,
    /// Free space on the cache drive.
    pub free: Option<u64>,
}

impl SsdUsage {
    /// What counts against the budget.
    pub fn used(&self) -> u64 {
        self.previews + self.kept + self.heads + self.other
    }
}

/// [`ssd_usage_in`] for the app's cache root.
pub fn ssd_usage(recordings: &Path) -> SsdUsage {
    ssd_usage_in(&super::cache_root(), recordings)
}

/// `recordings`: the clip folder, to find its `Edited` folder. That folder is read only when it
/// is on the cache's volume, so the hard drive is never touched.
pub(super) fn ssd_usage_in(root: &Path, recordings: &Path) -> SsdUsage {
    let inventory = budget::inventory(root, &|_| 0.0);
    let tier = |t: Tier| inventory.tier_bytes(t);
    let previews = tier(Tier::Thumb) + tier(Tier::Scrub) + tier(Tier::EditorMedia) + inventory.proxy;
    // Leftover copies count in the C: usage shown, though not in the budget (RI3-4).
    let kept = tier(Tier::Video) + inventory.staged + inventory.leaked;
    let other = (inventory.other + inventory.projects).saturating_sub(inventory.heads + inventory.staged + inventory.proxy);
    let pre_encoded = budget::dir_size(&root.join("tmp").join("pre-encode")).0;
    SsdUsage {
        previews,
        kept,
        heads: inventory.heads,
        other: other + pre_encoded,
        budget: super::video::budget_in(root),
        exports: edited_on(root, recordings),
        free: super::drive_info(root).map(|info| info.free),
    }
}

/// `Edited` when it shares the cache's volume (the SSD row counts it), else `None`.
fn edited_dir_on(root: &Path, recordings: &Path) -> Option<std::path::PathBuf> {
    let edited = super::export_dir(&recordings.join("clip.mp4"), recordings)?;
    let cache_volume = super::drive::volume_root(root);
    (cache_volume.is_some() && super::drive::volume_root(&edited) == cache_volume).then_some(edited)
}

/// The folders whose contents the SSD row counts: the cache, plus `Edited` when it is on the
/// cache's drive. Watched for outside deletes (`library::cache_watch`).
pub fn counted_dirs(recordings: &Path) -> Vec<std::path::PathBuf> {
    let root = super::cache_root();
    let edited = edited_dir_on(&root, recordings);
    std::iter::once(root).chain(edited).collect()
}

/// Another program removed files the SSD row counts: walk again (at most every few seconds).
pub fn note_outside_change() {
    super::prefetch::invalidate_usage();
}

/// Bytes of finished exports in `Edited` when it shares the cache's volume, else 0.
fn edited_on(root: &Path, recordings: &Path) -> u64 {
    let Some(edited) = edited_dir_on(root, recordings) else { return 0 };
    let Ok(listing) = std::fs::read_dir(&edited) else { return 0 };
    let mut total = 0;
    for entry in listing.flatten().filter(|entry| !entry.file_name().to_string_lossy().starts_with('.')) {
        let Ok(metadata) = entry.metadata() else { continue };
        if metadata.is_file() {
            total += metadata.len();
        } else if metadata.is_dir() {
            // A game's exports ("Sort clips into game folders": `Edited\<Game>\`), one level down.
            total += files_in(&entry.path());
        }
    }
    total
}

/// Bytes of the visible files directly in `folder`.
fn files_in(folder: &Path) -> u64 {
    let Ok(listing) = std::fs::read_dir(folder) else { return 0 };
    listing
        .flatten()
        .filter(|entry| !entry.file_name().to_string_lossy().starts_with('.'))
        .filter_map(|entry| entry.metadata().ok())
        .filter(|metadata| metadata.is_file())
        .map(|metadata| metadata.len())
        .sum()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(path: &Path, bytes: usize) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, vec![7u8; bytes]).unwrap();
    }

    #[test]
    fn splits_the_cache_into_previews_kept_copies_and_heads() {
        let dir = crate::test_temp::TempDir::new(format!("recall-ssd-usage-{}", std::process::id()));
        let root = dir.join("cache");
        write(&root.join("thumbs").join("aaaaaaaaaaaaaaaa.jpg"), 100);
        write(&root.join("visuals").join("aaaaaaaaaaaaaaaa").join("scrub-0.jpg"), 200);
        write(&root.join("visuals").join("aaaaaaaaaaaaaaaa").join("poster.jpg"), 300);
        write(&root.join("proxy").join("p.mp4"), 400);
        write(&root.join("video").join("aaaaaaaaaaaaaaaa.mp4"), 5000);
        write(&root.join("light").join("bbbbbbbbbbbbbbbb.mp4"), 600);
        write(&root.join("staging").join("kept").join("c.mp4"), 700);
        write(&root.join("heads").join("a.head"), 800);
        write(&root.join("projects").join("a.json"), 10);
        write(&root.join("tmp").join("pre-encode").join("x.mp4"), 900);
        std::fs::write(root.join("storage.json"), br#"{ "cache_budget_bytes": 2147483648 }"#).unwrap();
        let settings = root.join("storage.json").metadata().unwrap().len();
        // Recordings in `<dir>\Recall\Raw`: exports land in `<dir>\Recall\Edited`, same volume here.
        let raw = dir.join("Recall").join("Raw");
        write(&raw.join("clip.mp4"), 99_999);
        write(&dir.join("Recall").join("Edited").join("clip-edited-1.mp4"), 1000);
        write(&dir.join("Recall").join("Edited").join(".clip-edited-2.mp4.delivering"), 55);

        let usage = ssd_usage_in(&root, &raw);
        assert_eq!(usage.previews, 100 + 200 + 300 + 400);
        assert_eq!(usage.kept, 5000 + 600 + 700);
        assert_eq!(usage.heads, 800);
        // The ownership list (`owned.rs`) the inventory wrote when it adopted this cache.
        let listed = root.join("owned.json").metadata().map_or(0, |m| m.len());
        assert_eq!(usage.other, 10 + settings + listed + 900, "the pre-encode is cache work");
        assert_eq!(usage.budget, 2 << 30);
        assert_eq!(usage.exports, 1000, "finished exports, no hidden part files");
        assert_eq!(usage.used(), 1000 + 6300 + 800 + 10 + settings + listed + 900, "the pre-encode is held against the budget");
        assert!(raw.join("clip.mp4").exists() && dir.join("Recall").join("Edited").join("clip-edited-1.mp4").exists(), "read only");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn invalidating_the_totals_moves_the_generation() {
        let before = generation();
        super::super::prefetch::invalidate_usage();
        assert!(generation() > before);
    }
}
