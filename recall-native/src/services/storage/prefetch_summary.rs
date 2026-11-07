//! Cheap per-tier cache totals for Settings (split from prefetch.rs).
use super::*;

/// Cheap per-tier totals for Settings, refreshed in the background when stale.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(in crate::services::storage) struct Summary {
    pub thumbs: u64,
    pub scrub: u64,
    pub editor: u64,
    pub video: u64,
    pub total: u64,
    /// Clips with more than a thumbnail cached.
    pub preloaded: usize,
}

static SUMMARY_STALE: AtomicBool = AtomicBool::new(true);
static SUMMARY_REFRESHING: AtomicBool = AtomicBool::new(false);

fn summary_cell() -> &'static Mutex<Option<Summary>> {
    static SUMMARY: OnceLock<Mutex<Option<Summary>>> = OnceLock::new();
    SUMMARY.get_or_init(|| Mutex::new(None))
}

pub(in crate::services::storage) fn invalidate_usage() {
    SUMMARY_STALE.store(true, Ordering::Relaxed);
    super::super::ssd_usage::bump();
}

pub(in crate::services::storage) fn compute_summary(root: &Path) -> Summary {
    let inventory = budget::inventory(root, &|_| 0.0);
    let media = budget::read_media(root);
    let preloaded: HashSet<&str> =
        inventory.units.iter().filter(|u| u.tier != Tier::Thumb).filter_map(|u| budget::unit_clip(&u.name, &media)).collect();
    Summary {
        thumbs: inventory.tier_bytes(Tier::Thumb),
        scrub: inventory.tier_bytes(Tier::Scrub),
        editor: inventory.tier_bytes(Tier::EditorMedia),
        video: inventory.tier_bytes(Tier::Video),
        total: inventory.total(),
        preloaded: preloaded.len(),
    }
}

/// Latest totals; the first call walks the cache, later ones never block (a stale snapshot
/// is refreshed on a background thread).
pub(in crate::services::storage) fn summary() -> Summary {
    let current = *summary_cell().lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    let Some(current) = current else {
        let fresh = compute_summary(&super::super::cache_root());
        SUMMARY_STALE.store(false, Ordering::Relaxed);
        *summary_cell().lock().unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(fresh);
        return fresh;
    };
    if SUMMARY_STALE.load(Ordering::Relaxed) && !SUMMARY_REFRESHING.swap(true, Ordering::AcqRel) {
        SUMMARY_STALE.store(false, Ordering::Relaxed);
        let _ = std::thread::Builder::new().name("recall-cache-usage".into()).spawn(|| {
            let fresh = compute_summary(&super::super::cache_root());
            *summary_cell().lock().unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(fresh);
            SUMMARY_REFRESHING.store(false, Ordering::Release);
        });
    }
    current
}
