//! The Library's game filter tells the smart cache what is likely next: selecting a game
//! fetches the heads of its likeliest clips (`storage::probe_filter`), leaving it cancels them.
use crate::services::{
    library::{ClipRecord, Filter},
    storage::{self, ProbeClip},
};

/// The clips of `game` as the probe sees them.
pub fn clips_of(records: &[ClipRecord], game: &str) -> Vec<ProbeClip> {
    records.iter().filter(|r| r.game_key == game).map(|r| ProbeClip { path: r.path.clone(), bytes: r.bytes, mtime_secs: r.mtime }).collect()
}

/// Starts or cancels the probe when the filtered game changed since `probed`.
pub fn follow(probed: &mut Option<String>, filter: &Filter, records: &[ClipRecord]) {
    if *probed == filter.game_key {
        return;
    }
    probed.clone_from(&filter.game_key);
    match &filter.game_key {
        Some(game) => {
            storage::probe_filter(clips_of(records, game));
        }
        None => storage::cancel_probe(),
    }
}
