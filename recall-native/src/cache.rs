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

/// storage.json is damaged and backed up: the size "Reset cache size" offers, and its source.
pub fn reset_offer() -> Option<storage::ResetOffer> {
    storage::reset_offer()
}

/// Writes the size the Reset button showed (the user's number when one could be recovered).
pub fn reset_budget(shown: u64) -> Result<u64, String> {
    storage::reset_budget(shown)
}

pub fn set_budget_bytes(bytes: u64) {
    storage::set_budget_bytes(bytes)
}

/// The recorder's leftover kept copies (`staging\kept\leaked`), if any.
pub fn leftovers() -> Option<storage::Leftovers> {
    storage::leftovers()
}

/// Sends the leftover copies shown to the Recycle Bin (never a permanent delete). Blocks: run
/// it on a worker (`Job::RecycleLeftovers`).
pub fn send_leftovers_to_bin(shown: &[std::path::PathBuf]) -> Result<usize, String> {
    storage::send_leftovers_to_bin(shown)
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
