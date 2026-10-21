//! Thin adapter between Settings and the storage service's SSD cache.
use std::path::PathBuf;

use crate::services::storage;

pub fn cache_root() -> PathBuf {
    storage::cache_root()
}

pub fn budget_bytes() -> u64 {
    storage::budget_bytes()
}

pub fn set_budget_bytes(bytes: u64) {
    storage::set_budget_bytes(bytes)
}

/// (label, bytes) per kind of cached data.
pub fn usage() -> Vec<(&'static str, u64)> {
    storage::usage()
}

/// Removes everything cached (all of it is rebuildable). Returns bytes freed.
pub fn clear() -> Result<u64, String> {
    storage::clear()
}

/// One human line about what the smart cache has preloaded.
pub fn smart_status() -> String {
    storage::smart_status()
}
