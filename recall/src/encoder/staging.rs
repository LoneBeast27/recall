//! SSD staging for replay saves. A 120 s / 400 MB clip took 9549 ms to mux
//! straight into the clip folder (measured on an SMR hard drive, ~42 MB/s;
//! 2026-10-23) while the encoder side took 11.8 ms. So when the clip folder is
//! on another volume, the saver muxes into `<staging>\<final name>.part`,
//! renames it to `<staging>\<final name>`, reports `saved` right away (`path`
//! = the FINAL path, `staged: true`, `staged_path`), and the `recall-mover`
//! thread (`mover.rs`) copies it into the clip folder in the background and
//! reports `clip_moved` / `clip_move_failed`.
//!
//! - Folder: `<directory of recall.exe>\cache\staging` (the installer keeps the
//!   install folder on an SSD); `RECALL_STAGING_DIR=<dir>` overrides it and
//!   `RECALL_STAGING_DIR=off` turns staging off.
//! - A save is staged only when the staging folder is on a different volume
//!   than its clip folder ([`same_volume`]); otherwise it is written in place.
//! - Per-clip sidecars are written against the FINAL path, straight into the
//!   clip folder: the bookmark sidecar (`bookmarks.rs`, on the `saved` event)
//!   and the gallery thumbnail + Moments row (`mux::finish_gallery`, which
//!   reads the request-time frame, never the clip file). Only the `.mp4`
//!   travels through staging.
//! - Crash recovery ([`recover`], at engine start, before the first save): every
//!   finished `*.mp4` in the staging folder is queued for moving to the current
//!   clip folder, and stale `*.mp4.part` mux files there are deleted. With
//!   staging off the default folder is still scanned, so nothing is stranded.
//!   Its `kept\` subfolder (copies of clips already moved, kept for the app's
//!   SSD video cache, `mover.rs`) is not scanned: the app adopts or deletes them.

use std::path::{Component, Path, PathBuf, Prefix};

use super::events::Events;
use super::mover::{self, MoveJob, MoverControl, MoverQueue};
use crate::naming::PART_EXT;

pub(crate) const ENV: &str = "RECALL_STAGING_DIR";

/// `fn(a, b) -> bool`: are the two folders on the same volume.
pub(crate) type VolumeCheck = fn(&Path, &Path) -> bool;

/// `<exe dir>\cache\staging`.
pub(crate) fn default_dir(exe: &Path) -> Option<PathBuf> {
    exe.parent().map(|dir| dir.join("cache").join("staging"))
}

/// The staging folder for new saves, from the `RECALL_STAGING_DIR` value and
/// the running exe; `None` = staging off.
pub(crate) fn configured(env: Option<&str>, exe: Option<&Path>) -> Option<PathBuf> {
    match env.map(str::trim) {
        Some(v) if v.eq_ignore_ascii_case("off") => None,
        Some(v) if !v.is_empty() => Some(PathBuf::from(v)),
        _ => exe.and_then(default_dir),
    }
}

/// The folder recovered at start: the configured one, else the default one.
pub(crate) fn recovery_dir(env: Option<&str>, exe: Option<&Path>) -> Option<PathBuf> {
    configured(env, exe).or_else(|| exe.and_then(default_dir))
}

/// Comparable volume identity: `C:` for drive paths, `\\server\share` for UNC
/// (case-folded); `None` when the path has no prefix to compare.
fn volume_key(path: &Path) -> Option<String> {
    let abs = std::path::absolute(path).ok()?;
    let Some(Component::Prefix(prefix)) = abs.components().next() else { return None };
    Some(match prefix.kind() {
        Prefix::Disk(d) | Prefix::VerbatimDisk(d) => format!("{}:", d.to_ascii_uppercase() as char),
        Prefix::UNC(server, share) | Prefix::VerbatimUNC(server, share) => {
            format!(r"\\{}\{}", server.to_string_lossy(), share.to_string_lossy()).to_lowercase()
        }
        _ => prefix.as_os_str().to_string_lossy().to_lowercase(),
    })
}

/// Same drive letter / UNC share. Folders mounted into another drive's tree
/// count as that drive, and an unknown answer counts as "same", so the worst
/// case is today's behaviour: write the clip in place.
pub(crate) fn same_volume(a: &Path, b: &Path) -> bool {
    match (volume_key(a), volume_key(b)) {
        (Some(x), Some(y)) => x == y,
        _ => true,
    }
}

/// Where a save of `final_path` is staged (`<dir>\<final file name>`), or
/// `None` to write it in place (same volume, or no file name).
pub(crate) fn staged_path(dir: &Path, final_path: &Path, same: VolumeCheck) -> Option<PathBuf> {
    let name = final_path.file_name()?;
    let out_dir = final_path.parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or(Path::new("."));
    (!same(dir, out_dir)).then(|| dir.join(name))
}

/// What a start-up scan of the staging folder found.
#[derive(Debug, Default)]
pub(crate) struct Recovery {
    /// (staged clip, destination in the clip folder), oldest first.
    pub moves: Vec<(PathBuf, PathBuf)>,
    /// Stale `.part` mux files that were deleted.
    pub removed: Vec<PathBuf>,
    pub errors: Vec<String>,
}

/// Scan `dir` once (see the module doc). Only `*.mp4` and `*.mp4.part` names
/// are touched, so an override pointing at a shared folder moves nothing else.
pub(crate) fn recover(dir: &Path, out_dir: &Path) -> Recovery {
    let mut found = Recovery::default();
    let Ok(listing) = std::fs::read_dir(dir) else { return found };
    let mut clips = Vec::new();
    for entry in listing.flatten() {
        if !entry.file_type().is_ok_and(|t| t.is_file()) {
            continue;
        }
        let name = entry.file_name();
        let lower = name.to_string_lossy().to_ascii_lowercase();
        let path = entry.path();
        if lower.strip_suffix(PART_EXT).is_some_and(|stem| stem.ends_with(".mp4")) {
            match std::fs::remove_file(&path) {
                Ok(()) => found.removed.push(path),
                Err(e) => found.errors.push(format!("stale {} not removed: {e}", path.display())),
            }
        } else if lower.ends_with(".mp4") {
            let modified = entry.metadata().and_then(|m| m.modified()).ok();
            clips.push((modified, path, out_dir.join(&name)));
        }
    }
    clips.sort();
    found.moves = clips.into_iter().map(|(_, staged, dest)| (staged, dest)).collect();
    found
}

/// What the saver needs to stage a save: the folder and the mover's queue.
pub(crate) struct Stage {
    pub dir: PathBuf,
    pub queue: MoverQueue,
    pub same_volume: VolumeCheck,
}

impl Stage {
    /// Where this save is written: `Some(staged file)`, or `None` for in place
    /// (same volume, or a staging folder that cannot be created).
    pub fn target(&self, final_path: &str, events: &Events) -> Option<PathBuf> {
        let staged = staged_path(&self.dir, Path::new(final_path), self.same_volume)?;
        match std::fs::create_dir_all(&self.dir) {
            Ok(()) => Some(staged),
            Err(e) => {
                events.log(&format!("staging folder {} unavailable ({e}); writing the clip in place", self.dir.display()));
                None
            }
        }
    }

    /// Queue the finished staged clip for its background move.
    pub fn hand_off(&self, staged: &Path, final_path: &str, events: &Events) {
        let job = MoveJob { staged: staged.to_path_buf(), dest: PathBuf::from(final_path) };
        if !self.queue.submit(job) {
            events.log(&format!("clip mover stopped; {} stays staged until the next start", staged.display()));
        }
    }
}

/// How the saver's staging is set up at spawn.
pub(crate) struct Setup {
    /// Staging folder for new saves (`None` = off).
    pub dir: Option<PathBuf>,
    /// Folder whose leftovers are recovered now.
    pub recover: Option<PathBuf>,
    /// The clip folder recovered clips move to.
    pub out_dir: PathBuf,
    pub mover: mover::Config,
}

impl Setup {
    /// Production setup: `RECALL_STAGING_DIR` / the exe's folder and `RECALL_OUT_DIR`.
    pub fn from_env() -> Self {
        let env = std::env::var(ENV).ok();
        let exe = std::env::current_exe().ok();
        Setup {
            dir: configured(env.as_deref(), exe.as_deref()),
            recover: recovery_dir(env.as_deref(), exe.as_deref()),
            out_dir: PathBuf::from(crate::naming::out_dir()),
            mover: mover::Config::default(),
        }
    }
}

/// Recover leftovers, then start the mover. Returns the saver's [`Stage`]
/// (`None`: staging off, or the mover could not start, so saves go in place)
/// and the mover's quit handle.
pub(crate) fn start(events: &Events, setup: Setup) -> (Option<Stage>, Option<MoverControl>) {
    let mut recovered = Vec::new();
    if let Some(dir) = &setup.recover {
        let found = recover(dir, &setup.out_dir);
        for path in &found.removed {
            events.log(&format!("staging: removed stale {}", path.display()));
        }
        for error in &found.errors {
            events.log(&format!("staging: {error}"));
        }
        if !found.moves.is_empty() {
            let (n, out) = (found.moves.len(), setup.out_dir.display());
            events.log(&format!("staging: {n} finished clip(s) from an earlier run queued for moving to {out}"));
        }
        recovered = found.moves.into_iter().map(|(staged, dest)| MoveJob { staged, dest }).collect();
    }
    if setup.dir.is_none() && recovered.is_empty() {
        return (None, None);
    }
    let same_volume = setup.mover.same_volume;
    let Some((queue, control)) = mover::spawn(events.clone(), setup.mover, recovered) else {
        return (None, None);
    };
    let stage = setup.dir.map(|dir| {
        let mode = if same_volume(&dir, &setup.out_dir) { "off (same volume as the clip folder)" } else { "on" };
        events.log(&format!("staging {mode}: {} -> {}", dir.display(), setup.out_dir.display()));
        Stage { dir, queue, same_volume }
    });
    (stage, Some(control))
}

#[cfg(test)]
#[path = "staging_tests.rs"]
mod tests;
