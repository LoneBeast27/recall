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
//!   It is also written in place when the staging drive is short of space or
//!   fills up mid-mux (`space.rs`).
//! - Per-clip sidecars are written against the FINAL path, straight into the
//!   clip folder: the bookmark sidecar (`bookmarks.rs`, on the `saved` event)
//!   and the gallery thumbnail (`mux::finish_gallery`, which
//!   reads the request-time frame, never the clip file). Only the `.mp4`
//!   travels through staging.
//! - Crash recovery ([`recover`], at engine start, before the first save): every
//!   finished `*.mp4` in the staging folder is queued for moving to the current
//!   clip folder; an unfinished `*.mp4.part` mux file there is left alone (a save cut off is
//!   recovered from the in-progress list, `longrec::recover`; an older build's once, by the
//!   one-time scan). With staging off the default folder is still scanned, so nothing is stranded.
//!   Its `kept\` subfolder (copies of clips already moved, kept for the app's
//!   SSD video cache, `mover.rs`) is not scanned: the app adopts or deletes them. The one
//!   exception: a kept copy whose clip is only a same-length `.part` in the clip folder (a
//!   power cut lost the rename) is handed back and queued again (`staging_kept.rs`), on its
//!   own thread once the mover runs: that check reads the clip folder.

use std::path::{Component, Path, PathBuf, Prefix};

use super::events::Events;
use super::mover::{self, MoveJob, MoverControl, MoverQueue};

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
pub(crate) fn volume_key(path: &Path) -> Option<String> {
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

/// Same volume, as Windows resolves it (`win_volume.rs`: junctions, mounted
/// folders and subst drives count as the volume they land on). When Windows
/// cannot say, the drive letter / UNC share text decides, and an unknown
/// answer counts as "same", so the worst case is writing the clip in place.
pub(crate) fn same_volume(a: &Path, b: &Path) -> bool {
    // Cached per folder on this thread: the saver asks once per save, the mover once per move.
    if let (Some(x), Some(y)) = (crate::win_volume::volume_of_cached(a), crate::win_volume::volume_of_cached(b)) {
        return x.id == y.id;
    }
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

/// Why `dir` may not be the staging folder: it is, holds or lies inside one of `clip_dirs`.
/// Recovery would then queue the user's clips as leftovers and delete their `.part` files.
pub(crate) fn refused(dir: &Path, clip_dirs: &[PathBuf]) -> Option<String> {
    let staging = super::staging_guard::normalized(dir);
    clip_dirs.iter().filter(|c| !c.as_os_str().is_empty()).find_map(|clips| {
        let key = super::staging_guard::normalized(clips);
        // The text, or the files themselves (`\\nas\x` and `\\10.0.0.5\x`, junctions).
        let how = if super::staging_guard::within(&staging, &key) || crate::file_id::inside(dir, clips) == Some(true) {
            "is inside the clip folder"
        } else if super::staging_guard::within(&key, &staging) || crate::file_id::inside(clips, dir) == Some(true) {
            "holds the clip folder"
        } else {
            return None;
        };
        Some(format!("staging folder {} {how} {}: staging is off and that folder is not scanned, so nothing in it is moved or deleted", dir.display(), clips.display()))
    })
}

/// What a start-up scan of the staging folder found.
#[derive(Debug, Default)]
pub(crate) struct Recovery {
    /// (staged clip, destination in the clip folder), oldest first.
    pub moves: Vec<(PathBuf, PathBuf)>,
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
        // `.part` files are `longrec::recover`'s (the in-progress list, the one-time scan).
        if lower.ends_with(".mp4") {
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
    /// Folders refused as overlapping a clip folder ([`refused`]), logged at start.
    pub refused: Vec<String>,
}

impl Setup {
    /// Production setup: `RECALL_STAGING_DIR` / the exe's folder and `RECALL_OUT_DIR`.
    pub fn from_env() -> Self {
        let env = std::env::var(ENV).ok();
        let exe = std::env::current_exe().ok();
        let out_dir = PathBuf::from(crate::naming::out_dir());
        let mut clip_dirs = vec![out_dir.clone()];
        clip_dirs.extend(super::staging_guard::library_dirs());
        Setup {
            dir: configured(env.as_deref(), exe.as_deref()),
            recover: recovery_dir(env.as_deref(), exe.as_deref()),
            out_dir,
            mover: mover::Config::default(),
            refused: Vec::new(),
        }
        .guarded(&clip_dirs)
    }

    /// Drops a staging or recovery folder that overlaps one of `clip_dirs` ([`refused`]).
    pub fn guarded(mut self, clip_dirs: &[PathBuf]) -> Self {
        for slot in [&mut self.dir, &mut self.recover] {
            if let Some(why) = slot.as_deref().and_then(|dir| refused(dir, clip_dirs)) {
                if !self.refused.contains(&why) {
                    self.refused.push(why);
                }
                *slot = None;
            }
        }
        self
    }
}

/// Recover leftovers, then start the mover. Returns the saver's [`Stage`]
/// (`None`: staging off, or the mover could not start, so saves go in place)
/// and the mover's quit handle.
pub(crate) fn start(events: &Events, setup: Setup) -> (Option<Stage>, Option<MoverControl>) {
    let mut recovered = Vec::new();
    for why in &setup.refused {
        events.log(&format!("staging: {why}"));
    }
    if let Some(dir) = &setup.recover {
        let found = recover(dir, &setup.out_dir);
        for error in &found.errors {
            events.log(&format!("staging: {error}"));
        }
        if !found.moves.is_empty() {
            let (n, out) = (found.moves.len(), setup.out_dir.display());
            events.log(&format!("staging: {n} finished clip(s) from an earlier run queued for moving to {out}"));
        }
        recovered = found.moves.into_iter().map(|(staged, dest)| MoveJob { staged, dest }).collect();
    }
    let kept_check = setup.recover.clone().filter(|dir| super::staging_kept::holds_kept(dir));
    if setup.dir.is_none() && recovered.is_empty() && kept_check.is_none() {
        return (None, None);
    }
    let same_volume = setup.mover.same_volume;
    let Some((queue, control)) = mover::spawn(events.clone(), setup.mover, recovered) else {
        return (None, None);
    };
    if let Some(dir) = kept_check {
        super::staging_kept::hand_back_later(events.clone(), queue.clone(), dir, setup.out_dir.clone());
    }
    // No volume question here: on a sleeping NAS it would hold the encoder's
    // start. Each save asks it on the saver thread ([`Stage::target`]).
    let stage = setup.dir.map(|dir| {
        events.log(&format!("staging: {} -> {} (used when the clip folder is on another volume)", dir.display(), setup.out_dir.display()));
        Stage { dir, queue, same_volume }
    });
    (stage, Some(control))
}

#[cfg(test)]
#[path = "staging_tests.rs"]
mod tests;
