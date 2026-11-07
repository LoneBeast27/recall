//! Keeping the staged copy of a moved clip for the app's SSD video cache (`mover.rs`):
//! `<staging>\kept\<final name>`, marked with [`KEPT_MARK`], bounded by [`kept_cap`].

use std::path::{Path, PathBuf};

/// `<staging>\kept`: staged copies of moved clips, waiting for the app.
/// `recover` scans only the staging folder's own files, so these never move again.
pub(crate) const KEPT_DIR: &str = "kept";

/// The app's first-run cache budget on the system drive (2 GiB, `recall-native` `evict.rs`),
/// when `<cache>\storage.json` names none yet. `kept\` stays inside that budget.
const DEFAULT_CACHE_BUDGET: u64 = 2 << 30;

/// Most bytes `kept\` may hold: half the app's cache budget. `kept\` counts against that
/// budget and cannot be evicted, so at the whole budget (the old fixed 2 GiB) it left no
/// room and the app's budget pass emptied every other tier (2026-11-05).
pub(crate) fn kept_cap(staging: &Path) -> u64 {
    let budget = staging
        .parent()
        .and_then(|cache| std::fs::read(cache.join("storage.json")).ok())
        .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok())
        .and_then(|settings| settings.get("cache_budget_bytes").and_then(|v| v.as_u64()))
        .unwrap_or(DEFAULT_CACHE_BUDGET);
    budget / 2
}

/// Renames the staged copy to `<staging>\kept\<dest file name>`; `None` (the
/// caller deletes it) when that folder or the rename fails, or the copy would
/// take `kept\` past [`kept_cap`].
pub(crate) fn keep_staged(staged: &Path, dest: &Path) -> Option<PathBuf> {
    keep_staged_within(staged, dest, kept_cap(staged.parent()?))
}

pub(crate) fn keep_staged_within(staged: &Path, dest: &Path, cap: u64) -> Option<PathBuf> {
    let dir = staged.parent()?.join(KEPT_DIR);
    let kept = dir.join(dest.file_name()?);
    let held: u64 = std::fs::read_dir(&dir).into_iter().flatten().flatten().filter_map(|e| e.metadata().ok()).filter(|m| m.is_file()).map(|m| m.len()).sum();
    if held.saturating_add(std::fs::metadata(staged).ok()?.len()) > cap {
        return None;
    }
    std::fs::create_dir_all(&dir).ok()?;
    std::fs::rename(staged, &kept).ok()?;
    // The app's kept sweep deletes only marked copies; unmarked ones it only adopts.
    let _ = std::fs::write(format!("{}:{KEPT_MARK}", kept.display()), b"");
    Some(kept)
}

/// Alternate stream on each kept copy: "the engine kept this" (`recall-native` `kept.rs`).
pub(crate) const KEPT_MARK: &str = "recall.kept";
