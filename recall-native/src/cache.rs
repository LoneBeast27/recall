//! Thin adapter between the UI and the storage service's SSD cache.
//! Until `services::storage` lands, these are local stand-ins with the same names and
//! behaviour: cache root beside the exe (the installer guarantees an SSD), a persisted
//! budget (default 2 GB), directory-size usage and a full clear.
use std::path::{Path, PathBuf};

const DEFAULT_BUDGET: u64 = 2_000_000_000;

pub fn cache_root() -> PathBuf {
    if let Some(dir) = std::env::var_os("RECALL_CACHE_DIR") {
        return PathBuf::from(dir);
    }
    std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(|p| p.join("cache")))
        .unwrap_or_else(|| std::env::temp_dir().join("Recall").join("cache"))
}

fn budget_file() -> PathBuf {
    cache_root().join("budget.json")
}

pub fn budget_bytes() -> u64 {
    std::fs::read_to_string(budget_file())
        .ok()
        .and_then(|text| serde_json::from_str::<serde_json::Value>(&text).ok())
        .and_then(|v| v.get("budget_bytes").and_then(|b| b.as_u64()))
        .unwrap_or(DEFAULT_BUDGET)
}

pub fn set_budget_bytes(bytes: u64) {
    let _ = std::fs::create_dir_all(cache_root());
    let _ = std::fs::write(budget_file(), serde_json::json!({ "budget_bytes": bytes }).to_string());
}

/// (label, bytes) per kind of cached data.
pub fn usage() -> Vec<(&'static str, u64)> {
    vec![("Cache", dir_size(&cache_root()))]
}

/// Removes everything cached (all of it is rebuildable). Returns bytes freed.
pub fn clear() -> Result<u64, String> {
    let root = cache_root();
    let freed = dir_size(&root);
    let budget = budget_bytes();
    if root.is_dir() {
        for entry in std::fs::read_dir(&root).map_err(|e| e.to_string())?.flatten() {
            let path = entry.path();
            let result = if path.is_dir() { std::fs::remove_dir_all(&path) } else { std::fs::remove_file(&path) };
            result.map_err(|e| format!("{}: {e}", path.display()))?;
        }
    }
    set_budget_bytes(budget);
    Ok(freed)
}

fn dir_size(path: &Path) -> u64 {
    let Ok(entries) = std::fs::read_dir(path) else { return 0 };
    entries
        .flatten()
        .map(|e| match e.metadata() {
            Ok(m) if m.is_dir() => dir_size(&e.path()),
            Ok(m) => m.len(),
            Err(_) => 0,
        })
        .sum()
}
