//! Thin adapter between Settings and the storage service's SSD cache.
use std::path::PathBuf;

use crate::services::storage;

pub fn cache_root() -> PathBuf {
    storage::cache_root()
}

pub fn budget_bytes() -> u64 {
    storage::budget_bytes()
}

/// Why the cache size is read-only right now, if it is.
pub fn budget_note() -> Option<String> {
    storage::budget_note()
}

/// storage.json is damaged and backed up: "Reset cache size" may be offered.
pub fn budget_resettable() -> bool {
    storage::budget_resettable()
}

/// Writes a fresh storage.json with the last budget read, else the first-run pick.
pub fn reset_budget() -> Result<u64, String> {
    storage::reset_budget()
}

pub fn set_budget_bytes(bytes: u64) {
    storage::set_budget_bytes(bytes)
}

/// The recorder's leftover kept copies (`staging\kept\leaked`), if any.
pub fn leftovers() -> Option<storage::Leftovers> {
    storage::leftovers()
}

/// Sends the leftover copies to the Recycle Bin (never a permanent delete).
pub fn send_leftovers_to_bin() -> Result<usize, String> {
    storage::send_leftovers_to_bin()
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
