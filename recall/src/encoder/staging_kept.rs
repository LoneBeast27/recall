//! Start-up recovery of a kept copy whose clip never landed (`staging.rs` [`super::staging::recover`]).
//!
//! The mover renames `<clip>.part` to `<clip>` on the clip folder's volume, flushes it there,
//! then keeps the staged copy as `<staging>\kept\<clip>` (`mover.rs`, `mover_flush.rs`). A move
//! made before that flush existed, or a flush Windows did not honour, can lose the rename to a
//! power cut while the kept copy survives: the clip folder then holds only `<clip>.part`, with
//! the clip's full length (its space is reserved up front, so the length alone does not prove
//! the bytes are whole). Such a kept copy is handed back to the staging folder and queued as a
//! move, so the mover copies it again over that `.part`. Nothing is deleted here, and the clip
//! folder is only read.

use std::path::{Path, PathBuf};

use super::events::Events;
use super::mover::{MoveJob, MoverQueue};
use super::mover_keep::KEPT_DIR;
use crate::naming::part_of;

/// `<dir>\kept` holds a clip copy (a local SSD listing, cheap at start).
pub(crate) fn holds_kept(dir: &Path) -> bool {
    std::fs::read_dir(dir.join(KEPT_DIR)).is_ok_and(|mut listing| listing.any(|e| e.is_ok_and(|e| e.file_name().to_string_lossy().to_ascii_lowercase().ends_with(".mp4"))))
}

/// [`hand_back`] on its own thread (it stats the clip folder, which may be a sleeping NAS),
/// queueing each copy handed back as a move to `out_dir`.
pub(crate) fn hand_back_later(events: Events, queue: MoverQueue, dir: PathBuf, out_dir: PathBuf) {
    let _ = std::thread::Builder::new().name("recall-kept-check".into()).spawn(move || {
        let (back, errors) = hand_back(&dir, &out_dir);
        for error in errors {
            events.log(&format!("staging: {error}"));
        }
        for staged in back {
            let Some(name) = staged.file_name() else { continue };
            let dest = out_dir.join(name);
            events.log(&format!("staging: {} was kept but its clip is only a .part; moving it again", dest.display()));
            if !queue.submit(MoveJob { staged: staged.clone(), dest }) {
                events.log(&format!("clip mover stopped; {} stays staged until the next start", staged.display()));
            }
        }
    });
}

/// Moves each kept copy of `dir` whose clip in `out_dir` is missing while a same-length
/// `<clip>.part` sits there back into `dir`. Returns (handed back, errors).
pub(crate) fn hand_back(dir: &Path, out_dir: &Path) -> (Vec<PathBuf>, Vec<String>) {
    let (mut back, mut errors) = (Vec::new(), Vec::new());
    let Ok(listing) = std::fs::read_dir(dir.join(KEPT_DIR)) else { return (back, errors) };
    for entry in listing.flatten() {
        let name = entry.file_name();
        let is_clip = name.to_string_lossy().to_ascii_lowercase().ends_with(".mp4");
        let Some(len) = entry.metadata().ok().filter(|m| m.is_file() && is_clip).map(|m| m.len()) else { continue };
        let dest = out_dir.join(&name);
        // The clip landed, or the clip folder cannot say (asleep, offline): nothing to do.
        if !matches!(dest.try_exists(), Ok(false)) {
            continue;
        }
        if !std::fs::metadata(part_of(&dest)).is_ok_and(|m| m.is_file() && m.len() == len) {
            continue;
        }
        let staged = dir.join(&name);
        if !matches!(staged.try_exists(), Ok(false)) {
            continue;
        }
        match std::fs::rename(entry.path(), &staged) {
            Ok(()) => back.push(staged),
            Err(e) => errors.push(format!("kept copy {} not handed back for its move: {e}", entry.path().display())),
        }
    }
    (back, errors)
}
