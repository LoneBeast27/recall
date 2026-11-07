//! `status.save_writing`: what the saver is writing now, read on the main thread at
//! each status line. One stat of the in-progress `.part` (0 bytes before the file
//! exists, e.g. while the audio still encodes), plus the saves queued behind it. A
//! controller holds engine restarts while the bytes keep growing (`saver.rs` aborts
//! the save in flight when the engine quits).

use std::sync::atomic::Ordering::Relaxed;

use crate::protocol::SaveWriting;

use super::SaverControl;

impl SaverControl {
    /// `None` when no save is in the saver.
    pub fn writing(&self) -> Option<SaveWriting> {
        let jobs = self.jobs.load(Relaxed);
        if jobs == 0 {
            return None;
        }
        let part = self.current.lock().unwrap_or_else(|p| p.into_inner()).clone();
        let bytes = part.and_then(|p| std::fs::metadata(p).ok()).map_or(0, |m| m.len());
        Some(SaveWriting { bytes, queued: jobs - 1 })
    }
}
