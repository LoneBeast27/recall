//! Cache usage per tier ("Cache: 1.2 GB of 2 GB") and the Settings "clear" action.
use std::path::Path;

use super::budget;
use super::evict::Tier;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CacheKind {
    /// `thumbs/`: library thumbnails (~25 KB each; kept unless they alone exceed the budget).
    Thumbs,
    /// `visuals/<id>/scrub-*`: hover-scrub frames.
    ScrubFrames,
    /// `editor/` audio stems + `visuals/<id>/` poster and filmstrip of opened clips.
    EditorMedia,
    /// `video/`: SSD copies of opened clips (at most half the budget; evicted first).
    Video,
}

impl CacheKind {
    pub const ALL: [CacheKind; 4] = [CacheKind::Video, CacheKind::EditorMedia, CacheKind::ScrubFrames, CacheKind::Thumbs];

    pub fn label(self) -> &'static str {
        match self {
            CacheKind::Thumbs => "Library thumbnails",
            CacheKind::ScrubFrames => "Hover previews",
            CacheKind::EditorMedia => "Editor media",
            CacheKind::Video => "Video copies",
        }
    }

    fn tier(self) -> Tier {
        match self {
            CacheKind::Thumbs => Tier::Thumb,
            CacheKind::ScrubFrames => Tier::Scrub,
            CacheKind::EditorMedia => Tier::EditorMedia,
            CacheKind::Video => Tier::Video,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TierUsage {
    pub kind: CacheKind,
    pub bytes: u64,
    /// Cache entries (one per video copy, opened clip, clip with scrub frames, thumbnail).
    pub items: u64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct CacheUsage {
    /// Cache root folder (on the SSD).
    pub root: String,
    /// In [`CacheKind::ALL`] order.
    pub tiers: Vec<TierUsage>,
    /// Working edit projects (never evicted or cleared: they may hold unflushed edits).
    pub projects_bytes: u64,
    /// Settings, indexes and in-flight temp files.
    pub other_bytes: u64,
    /// Total cache budget (all tiers).
    pub budget: u64,
    /// Most the video tier may hold (half the budget).
    pub video_cap: u64,
}

impl CacheUsage {
    /// Everything the cache folder holds.
    pub fn total(&self) -> u64 {
        self.tiers.iter().map(|t| t.bytes).sum::<u64>() + self.projects_bytes + self.other_bytes
    }
}

/// Walks the cache (thousands of small files: call from a worker thread). The cheap
/// Settings rows are `storage::usage()`.
pub fn usage_detail() -> CacheUsage {
    usage_in(&super::cache_root())
}

pub(super) fn usage_in(root: &Path) -> CacheUsage {
    let inventory = budget::inventory(root, &|_| 0.0);
    let tiers = CacheKind::ALL
        .iter()
        .map(|kind| {
            let units = inventory.units.iter().filter(|u| u.tier == kind.tier());
            let (bytes, items) = units.fold((0, 0), |(b, n), u| (b + u.bytes, n + 1));
            TierUsage { kind: *kind, bytes, items }
        })
        .collect();
    let budget = super::video::budget_in(root);
    CacheUsage {
        root: root.to_string_lossy().into_owned(),
        tiers,
        projects_bytes: inventory.projects,
        other_bytes: inventory.other,
        budget,
        video_cap: budget / 2,
    }
}

/// Deletes one tier, except what the clip open in the editor uses. Returns bytes freed.
/// After clearing `Thumbs`, reload the clip list so tiles re-request their thumbnails.
pub fn clear_kind(kind: CacheKind) -> Result<u64, String> {
    let freed = clear_in(&super::cache_root(), kind);
    super::prefetch::invalidate_usage();
    freed
}

pub(super) fn clear_in(root: &Path, kind: CacheKind) -> Result<u64, String> {
    let _guard = budget::INDEX_LOCK.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    let keep = budget::kept();
    let inventory = budget::inventory(root, &|_| 0.0);
    Ok(inventory.units.iter().filter(|u| u.tier == kind.tier() && !keep.contains(&u.name)).map(|u| budget::delete(root, u)).sum())
}
