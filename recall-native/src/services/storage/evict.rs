//! Eviction plan for the one cache budget, plus `storage.json` settings.
//!
//! User rulings (2026-10-22): one budget for everything; video copies take
//! at most half of it; the lowest-score clips lose their cache first, and for a given clip
//! the costliest-to-hold / cheapest-to-skip tier goes first (video copy, then editor media,
//! then hover frames); library thumbnails go only when they alone exceed the budget.
use std::collections::BTreeMap;
use std::path::Path;

use serde::{Deserialize, Serialize};

const GIB: u64 = 1 << 30;
/// The first-run budget off the system drive is at most this (2026-11-02: 20 GiB, so recently
/// opened clips and the idle warm-up have room)...
pub const DEFAULT_CACHE_BUDGET: u64 = 20 * GIB;
/// ...and at most this share of the cache drive's free space...
pub const DEFAULT_FREE_SHARE_PERCENT: u64 = 25;
/// ...and leaves the drive at least this share of its size free...
pub const DEFAULT_KEEP_FREE_PERCENT: u64 = 10;
/// ...but is never below this.
pub const MIN_CACHE_BUDGET: u64 = 2 * GIB;

/// The budget a first run picks (no `storage.json` yet) on a cache drive with `free` of
/// `total` bytes. On the system drive it is [`MIN_CACHE_BUDGET`] (Rohan 2026-11-03: "keep the
/// cache on C: at 2 GB"; the engine's `kept\` copies count inside it). Elsewhere: the smallest
/// of [`DEFAULT_CACHE_BUDGET`], [`DEFAULT_FREE_SHARE_PERCENT`] of `free`, and `free` less
/// [`DEFAULT_KEEP_FREE_PERCENT`] of `total`, in whole GiB, at least [`MIN_CACHE_BUDGET`].
/// A budget already in `storage.json` is never replaced by this.
pub(super) fn default_budget(free: u64, total: u64, system_drive: bool) -> u64 {
    if system_drive {
        return MIN_CACHE_BUDGET;
    }
    let percent = |bytes: u64, share: u64| (u128::from(bytes) * u128::from(share) / 100) as u64;
    let share = percent(free, DEFAULT_FREE_SHARE_PERCENT);
    let spare = free.saturating_sub(percent(total, DEFAULT_KEEP_FREE_PERCENT));
    (share.min(spare).min(DEFAULT_CACHE_BUDGET) / GIB * GIB).max(MIN_CACHE_BUDGET)
}

/// Evictable tiers, in per-clip eviction order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(super) enum Tier {
    Video,
    EditorMedia,
    Scrub,
    Thumb,
}

impl Tier {
    /// Keep weight: a unit's keep value is `clip score × weight`, evicted lowest first.
    /// Hover frames are cheap to hold and slow to miss, so they outlast editor media,
    /// which outlasts video copies.
    fn keep_weight(self) -> f64 {
        match self {
            Tier::Video => 1.0,
            Tier::EditorMedia => 2.0,
            Tier::Scrub => 4.0,
            Tier::Thumb => f64::INFINITY,
        }
    }
}

/// One evictable unit: a video copy, an editor-media folder, one clip's frames, a thumbnail.
#[derive(Debug, Clone, PartialEq)]
pub(super) struct Unit {
    pub tier: Tier,
    /// Stable key, e.g. `video/<id>.mp4`, `editor/<key>`, `visuals/<id>#editor`, `visuals/<id>#scrub`.
    pub name: String,
    pub bytes: u64,
    /// Unix milliseconds of the last use (tie-break).
    pub last_used: u64,
    /// The owning clip's smart-cache score (0 for clips no longer listed).
    pub score: f64,
}

/// Units to delete so the video tier plus `incoming_video` fits `video_cap` and everything
/// (`fixed` never-evicted bytes included) plus `incoming_video` fits `budget`. Thumbnails are
/// never chosen (about 30 KB each, and the library shows them; on 2026-11-05 a budget filled by
/// never-evicted files took every one), nor is anything in `keep`.
pub(super) fn plan(units: &[Unit], fixed: u64, budget: u64, video_cap: u64, incoming_video: u64, keep: &[String]) -> Vec<String> {
    let order = |a: &&Unit, b: &&Unit| {
        let thumbs = (a.tier == Tier::Thumb).cmp(&(b.tier == Tier::Thumb));
        let value = |u: &Unit| if u.tier == Tier::Thumb { u.score } else { u.score * u.tier.keep_weight() };
        thumbs
            .then(value(a).total_cmp(&value(b)))
            .then(a.tier.cmp(&b.tier))
            .then(a.last_used.cmp(&b.last_used))
            .then_with(|| a.name.cmp(&b.name))
    };
    let mut candidates: Vec<&Unit> = units.iter().filter(|u| u.tier != Tier::Thumb && !keep.contains(&u.name)).collect();
    candidates.sort_by(order);
    let mut evict = Vec::new();
    let mut video = units.iter().filter(|u| u.tier == Tier::Video).map(|u| u.bytes).sum::<u64>().saturating_add(incoming_video);
    let mut total = units.iter().map(|u| u.bytes).sum::<u64>().saturating_add(fixed).saturating_add(incoming_video);
    // Video cap first: lowest-score copies go until the tier fits.
    for unit in candidates.iter().filter(|u| u.tier == Tier::Video) {
        if video <= video_cap {
            break;
        }
        video = video.saturating_sub(unit.bytes);
        total = total.saturating_sub(unit.bytes);
        evict.push(unit.name.clone());
    }
    for unit in candidates {
        if total <= budget {
            break;
        }
        if evict.contains(&unit.name) {
            continue;
        }
        total = total.saturating_sub(unit.bytes);
        evict.push(unit.name.clone());
    }
    evict
}

/// Room for a speculative video copy of a clip scored `below`: the video copies to delete so
/// `incoming` fits under `video_cap` and the whole cache under `limit`, taking only copies
/// scored strictly lower (lowest first, never `keep`). `None` when that is not enough: then
/// nothing is deleted and the copy is skipped, so warming never pushes out a better clip.
pub(super) fn room_below(units: &[Unit], fixed: u64, limit: u64, video_cap: u64, incoming: u64, below: f64, keep: &[String]) -> Option<Vec<String>> {
    let mut video = units.iter().filter(|u| u.tier == Tier::Video).map(|u| u.bytes).sum::<u64>().saturating_add(incoming);
    let mut total = units.iter().map(|u| u.bytes).sum::<u64>().saturating_add(fixed).saturating_add(incoming);
    let mut lower: Vec<&Unit> = units.iter().filter(|u| u.tier == Tier::Video && u.score < below && !keep.contains(&u.name)).collect();
    lower.sort_by(|a, b| a.score.total_cmp(&b.score).then(a.last_used.cmp(&b.last_used)).then_with(|| a.name.cmp(&b.name)));
    let mut evict = Vec::new();
    for unit in lower {
        if video <= video_cap && total <= limit {
            break;
        }
        video = video.saturating_sub(unit.bytes);
        total = total.saturating_sub(unit.bytes);
        evict.push(unit.name.clone());
    }
    (video <= video_cap && total <= limit).then_some(evict)
}

/// `{ "<key>": value }` index files (`video/lru.json`, `media.json` sections).
pub(super) fn read_index(path: &Path) -> BTreeMap<String, u64> {
    std::fs::read(path).ok().and_then(|bytes| serde_json::from_slice(&bytes).ok()).unwrap_or_default()
}

pub(super) fn write_index(path: &Path, index: &BTreeMap<String, u64>) -> Result<(), String> {
    let bytes = serde_json::to_vec(index).map_err(|error| error.to_string())?;
    super::projects::write_atomic(path, &bytes)
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub(super) struct Settings {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache_budget_bytes: Option<u64>,
    /// `true` beside a budget the first run picked (`video::budget_in`); absent once the user
    /// sets one in Settings. A budget without it may be the user's and is never changed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub first_run: Option<bool>,
}

/// What `storage.json` held when it was read.
pub(super) enum Stored {
    /// No file: a first run.
    Absent,
    Read(Settings),
    /// Present but locked, unreadable or not JSON: the user's budget may be in it (a scan or a
    /// sync client holding it, a write still under way), so nothing writes over it; the caller
    /// uses the default in memory and the next read tries again. A file that is not JSON is
    /// copied to a dated `storage.json.bad-*` first (once; never on a lock).
    Unusable(String),
}

pub(super) fn load_settings(root: &Path) -> Stored {
    let path = root.join("storage.json");
    match std::fs::read(&path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Stored::Absent,
        Err(error) => Stored::Unusable(format!("storage.json could not be read ({error})")),
        Ok(bytes) => match serde_json::from_slice(&bytes) {
            Ok(settings) => Stored::Read(settings),
            Err(error) => {
                let kept = crate::services::durable::quarantine(&path, std::time::SystemTime::now(), crate::services::durable::Keep::Copy);
                Stored::Unusable(format!("storage.json is damaged ({error}); a copy is kept as {kept:?}"))
            }
        },
    }
}

/// The settings, or the defaults when there are none or they can't be read ([`load_settings`]).
pub(super) fn read_settings(root: &Path) -> Settings {
    match load_settings(root) {
        Stored::Read(settings) => settings,
        Stored::Absent | Stored::Unusable(_) => Settings::default(),
    }
}

pub(super) fn write_settings(root: &Path, settings: &Settings) -> Result<(), String> {
    let bytes = serde_json::to_vec_pretty(settings).map_err(|error| error.to_string())?;
    super::projects::write_atomic(&root.join("storage.json"), &bytes)
}
