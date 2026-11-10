//! Start-up check of the kept copies (`staging.rs` [`super::staging::recover`]). The recorder
//! is the only one that deletes a kept copy, because only it knows its own clip folder
//! (`out_dir`); the app's start sweep only adopts and reports (`recall-native` `kept.rs`).
//!
//! The mover renames `<clip>.part` to `<clip>` on the clip folder's volume, flushes it there,
//! then keeps the staged copy as `<staging>\kept\<clip>` (`mover.rs`, `mover_flush.rs`). For
//! each copy:
//! - the clip landed whole (present, readable, the copy's size, and the same bytes throughout:
//!   a flush Windows did not honour can leave a full-length clip with zeroed data) and the
//!   copy is marked as the recorder's own: the copy is deleted (the clip is the whole recording);
//! - the clip is missing while a `<clip>.part` of the copy's length sits there (a power cut
//!   lost the rename): the copy is handed back to staging and moved again over that `.part`;
//! - the clip is present at another size or with other bytes, or gone with no `.part` (deleted
//!   by the user): the copy is "leaked". It is moved into `kept\leaked\` (never deleted), logged, and no longer
//!   counts against `kept_cap`, so leaked copies can't silently stop new keeps;
//! - anything else (a clip folder asleep or unreadable, a clip that can't be opened or read
//!   right now, a `.part` of another length): left, and checked again at the next start.

use std::path::{Path, PathBuf};

use super::events::Events;
use super::mover::{MoveJob, MoverQueue};
use super::mover_keep::{KEPT_DIR, KEPT_MARK};
use crate::naming::part_of;

/// `kept\leaked`: copies that match no whole clip (see the module notes).
pub(crate) const LEAKED_DIR: &str = "leaked";

/// What [`hand_back`] did.
#[derive(Debug, Default)]
pub(crate) struct HandBack {
    /// Copies handed back to staging for their move.
    pub back: Vec<PathBuf>,
    /// Marked copies of clips that landed whole: deleted.
    pub deleted: usize,
    /// Copies moved into `kept\leaked\`, with their sizes.
    pub leaked: Vec<(PathBuf, u64)>,
    pub errors: Vec<String>,
}

/// `<dir>\kept` holds a clip copy (a local SSD listing, cheap at start).
pub(crate) fn holds_kept(dir: &Path) -> bool {
    std::fs::read_dir(dir.join(KEPT_DIR)).is_ok_and(|mut listing| listing.any(|e| e.is_ok_and(|e| e.file_name().to_string_lossy().to_ascii_lowercase().ends_with(".mp4"))))
}

/// [`hand_back`] on its own thread (it stats the clip folder, which may be a sleeping NAS),
/// queueing each copy handed back as a move to `out_dir`.
pub(crate) fn hand_back_later(events: Events, queue: MoverQueue, dir: PathBuf, out_dir: PathBuf) {
    let _ = std::thread::Builder::new().name("recall-kept-check".into()).spawn(move || {
        let done = hand_back(&dir, &out_dir);
        for error in &done.errors {
            events.log(&format!("staging: {error}"));
        }
        if !done.leaked.is_empty() {
            let bytes: u64 = done.leaked.iter().map(|(_, len)| len).sum();
            events.log(&format!(
                "staging: {} kept cop(ies), {} MiB, match no whole clip in {}; set aside in {}\\{LEAKED_DIR} (not deleted, not counted against the keep limit)",
                done.leaked.len(),
                bytes >> 20,
                out_dir.display(),
                dir.join(KEPT_DIR).display()
            ));
        }
        for staged in done.back {
            let Some(name) = staged.file_name() else { continue };
            let dest = out_dir.join(name);
            events.log(&format!("staging: {} was kept but its clip is only a .part; moving it again", dest.display()));
            if !queue.submit(MoveJob { staged: staged.clone(), dest }) {
                events.log(&format!("clip mover stopped; {} stays staged until the next start", staged.display()));
            }
        }
    });
}

/// The recorder marked `kept` as its own copy (`mover_keep.rs`).
fn marked(kept: &Path) -> bool {
    std::fs::metadata(format!("{}:{KEPT_MARK}", kept.display())).is_ok()
}

/// How the clip at `dest` compares with its kept copy of `len` bytes.
#[derive(Debug, PartialEq)]
enum Landed {
    /// Same size and the same bytes, all of them.
    Whole,
    /// Another size or other bytes: the copy may be the only whole version.
    Differs,
    /// Could not be stat'ed, opened or read now (a scan, an exclusive open): ask again later.
    Unknown,
}

fn landed(dest: &Path, kept: &Path, len: u64) -> Landed {
    let Ok(meta) = std::fs::metadata(dest) else { return Landed::Unknown };
    if !meta.is_file() || meta.len() != len {
        return Landed::Differs;
    }
    // The whole file, read shared: matching ends let a clip with a lost middle pass as whole.
    match crate::same_bytes::same_bytes(dest, kept, len) {
        Ok(true) => Landed::Whole,
        Ok(false) => Landed::Differs,
        Err(_) => Landed::Unknown,
    }
}

/// Checks each kept copy of `dir` against its clip in `out_dir` (see the module notes).
pub(crate) fn hand_back(dir: &Path, out_dir: &Path) -> HandBack {
    let mut done = HandBack::default();
    let kept_dir = dir.join(KEPT_DIR);
    let Ok(listing) = std::fs::read_dir(&kept_dir) else { return done };
    for entry in listing.flatten() {
        let name = entry.file_name();
        let is_clip = name.to_string_lossy().to_ascii_lowercase().ends_with(".mp4");
        let Some(len) = entry.metadata().ok().filter(|m| m.is_file() && is_clip).map(|m| m.len()) else { continue };
        let (kept, dest) = (entry.path(), out_dir.join(&name));
        match dest.try_exists() {
            Ok(true) => match landed(&dest, &kept, len) {
                Landed::Whole if marked(&kept) => match std::fs::remove_file(&kept) {
                    Ok(()) => done.deleted += 1,
                    Err(e) => done.errors.push(format!("kept copy {} of a landed clip not removed: {e}", kept.display())),
                },
                Landed::Whole => {}
                Landed::Differs => set_aside(&kept_dir, &kept, len, &mut done),
                // RI3-5: a clip that is fine but busy right now is not "leaked"; next start asks again.
                Landed::Unknown => {}
            },
            Ok(false) => match std::fs::metadata(part_of(&dest)) {
                Ok(part) if part.is_file() && part.len() == len => hand_back_one(dir, &kept, &name, &mut done),
                Ok(_) => {} // a `.part` of another length: the copy may be the only whole one
                Err(e) if e.kind() == std::io::ErrorKind::NotFound && std::fs::read_dir(out_dir).is_ok() => set_aside(&kept_dir, &kept, len, &mut done),
                Err(_) => {}
            },
            // The clip folder cannot say (asleep, offline): nothing to do.
            Err(_) => {}
        }
    }
    done
}

/// Renames `kept` back into staging, so the recovery queues it as a move again.
fn hand_back_one(dir: &Path, kept: &Path, name: &std::ffi::OsStr, done: &mut HandBack) {
    let staged = dir.join(name);
    if !matches!(staged.try_exists(), Ok(false)) {
        return;
    }
    match std::fs::rename(kept, &staged) {
        Ok(()) => done.back.push(staged),
        Err(e) => done.errors.push(format!("kept copy {} not handed back for its move: {e}", kept.display())),
    }
}

/// Moves a leaked copy into `kept\leaked\` (never over a file there).
fn set_aside(kept_dir: &Path, kept: &Path, len: u64, done: &mut HandBack) {
    let leaked = kept_dir.join(LEAKED_DIR);
    let Some(target) = kept.file_name().map(|n| leaked.join(n)) else { return };
    if !matches!(target.try_exists(), Ok(false)) {
        done.errors.push(format!("kept copy {} matches no whole clip; {} is taken, left in place", kept.display(), target.display()));
        return;
    }
    match std::fs::create_dir_all(&leaked).and_then(|()| std::fs::rename(kept, &target)) {
        Ok(()) => done.leaked.push((target, len)),
        Err(e) => done.errors.push(format!("kept copy {} matches no whole clip; not set aside: {e}", kept.display())),
    }
}
