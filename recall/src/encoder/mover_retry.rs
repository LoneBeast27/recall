//! A move whose clip landed in the clip folder but could not be flushed there (`mover.rs`,
//! `mover_flush.rs`). The staged copy stays on the SSD and the move is retried:
//!
//! - The retry looks for the clip where it landed (a `-recovered` name included), and a clip
//!   that landed without the staged file's time ([`Landed::unstamped`]) is matched by size and
//!   bytes only, so a retry never lands a second copy.
//! - After [`WARN_AFTER`] tries without a flush the user is told once (`clip_move_failed`).
//!   The staged copy is still never deleted before a good flush.
//! - A clip deleted from its folder (which is still there) before the retry was deleted by the
//!   user: it is not copied back. Its staged copy is set aside, unmarked, as
//!   `<staging>\kept\<name>`: the app adopts it only if the clip comes back (restored from the
//!   Recycle Bin) and never deletes it, and the start-up recovery never moves it again.

use std::path::{Path, PathBuf};
use std::time::Instant;

use super::{MoveError, MoveJob, Moved, Mover, Pending};

/// Unflushed tries before the user is warned (once).
pub(super) const WARN_AFTER: u32 = 3;

/// The warning's reason (shown in the app's toast and the controller's log).
pub(super) const UNFLUSHED_REASON: &str = "couldn't confirm the clip is on disk in the clip folder; its SSD copy is kept and the move is retried";

/// What an earlier try of this move left in the clip folder.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) struct Landed {
    /// The clip landed at the job's `dest`; only its flush failed.
    pub before: bool,
    /// It landed without the staged file's modification time.
    pub unstamped: bool,
}

/// `dest` holds the clip a try landed (see the module doc).
pub(super) fn landed_here(staged: &Path, dest: &Path, landed: Landed) -> bool {
    super::super::landed::same_clip(staged, dest) || (landed.unstamped && super::super::landed::same_bytes(staged, dest))
}

/// The landed clip is gone while its folder is there: the user deleted it. A missing folder
/// (the drive asleep or unplugged) is retried instead.
pub(super) fn deleted_since(dest: &Path, landed: Landed) -> Option<MoveError> {
    let folder = dest.parent().filter(|d| !d.as_os_str().is_empty()).unwrap_or(Path::new("."));
    if !landed.before {
        return None;
    }
    Some(if folder.is_dir() { MoveError::Deleted } else { MoveError::Failed(format!("the clip folder {} is not reachable", folder.display())) })
}

/// Renames the staged copy to `<staging>\kept\<name>`, unmarked (see the module doc).
pub(super) fn set_aside(staged: &Path) -> std::io::Result<PathBuf> {
    let (Some(dir), Some(name)) = (staged.parent(), staged.file_name()) else { return Err(std::io::Error::other("no folder")) };
    let dir = dir.join(super::super::mover_keep::KEPT_DIR);
    std::fs::create_dir_all(&dir)?;
    let to = dir.join(name);
    if to.try_exists()? {
        return Err(std::io::Error::new(std::io::ErrorKind::AlreadyExists, format!("{} exists", to.display())));
    }
    std::fs::rename(staged, &to)?;
    Ok(to)
}

impl Mover {
    /// Parks a move whose clip landed unflushed, remembering where and how it landed.
    pub(super) fn park_unflushed(&mut self, mut pending: Pending, moved: &Moved, why: &str) {
        let (dest, staged) = (moved.dest.to_string_lossy().into_owned(), pending.job.staged.to_string_lossy().into_owned());
        // The retry finds the clip in place, flushes again, then deletes the staged copy.
        self.events.log(&format!("clip mover: {dest} not flushed yet ({why}); {staged} stays staged for a retry"));
        pending.job.dest = moved.dest.clone();
        pending.landed = Landed { before: true, unstamped: pending.landed.unstamped || moved.unstamped.is_some() };
        pending.unflushed += 1;
        if pending.unflushed == WARN_AFTER {
            self.events.clip_move_failed(&dest, &staged, UNFLUSHED_REASON);
        }
        self.retry_at.get_or_insert(Instant::now() + self.cfg.retry_after);
        pending.announced = true;
        self.parked.push(pending);
    }

    /// The user deleted the landed clip before its flush retry: drop the move.
    pub(super) fn drop_deleted(&self, job: &MoveJob) {
        let dest = job.dest.display();
        match set_aside(&job.staged) {
            Ok(to) => self.events.log(&format!("clip mover: {dest} was deleted before its move finished; not copied back, its SSD copy set aside as {}", to.display())),
            Err(e) => self.events.log(&format!(
                "clip mover: {dest} was deleted before its move finished; not copied back ({} could not be set aside: {e})",
                job.staged.display()
            )),
        }
    }
}
