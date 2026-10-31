//! When a changed file in a clip folder may reach the library: pure logic, the clock and the
//! file checks are passed in (`watch.rs` does the I/O).
//!
//! A size check alone cannot tell: Windows copies set the final size first and fill the file
//! afterwards. A clip counts as finished when nothing has been heard about it for [`QUIET`], no
//! program holds it open for writing, and two checks [`RECHECK`] apart agree on size and mtime.
//! Results leave in batches at most every [`BATCH_GAP`], and at most [`MAX_PROBES`] files are
//! checked per pass, so a burst of files costs a few list updates rather than one per file.
use std::{
    collections::{BTreeMap, HashMap},
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

/// Nothing heard about a file for this long: time to look at it.
pub const QUIET: Duration = Duration::from_millis(400);
/// Gap between the two checks that must agree.
pub const RECHECK: Duration = Duration::from_millis(300);
/// Slowest re-check of a file a program still holds open. Stays well under the 3 s a finished
/// copy may take to show, for writers whose last event comes before they close the file.
pub const BUSY_MAX: Duration = Duration::from_secs(1);
/// A 0-byte clip waits this long: a copy may not have written anything yet.
pub const EMPTY_GRACE: Duration = Duration::from_secs(5);
/// Shortest gap between two batches.
pub const BATCH_GAP: Duration = Duration::from_millis(500);
/// Shortest gap between two passes of file checks.
pub const PASS_GAP: Duration = Duration::from_millis(100);
/// File checks per pass (metadata only, but each can seek on a hard drive).
pub const MAX_PROBES: usize = 128;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Clip,
    /// Not a clip by name: it may be a folder of clips arriving or leaving.
    Other,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Probe {
    Missing,
    /// Open for writing somewhere.
    Busy,
    Ready { bytes: u64, mtime: u64 },
    Directory,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Outcome {
    Ready(PathBuf, u64, u64),
    Gone(PathBuf),
    /// A file or folder left: everything listed under that path went with it.
    GoneUnder(PathBuf),
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct Batch {
    pub ready: Vec<(PathBuf, u64, u64)>,
    pub gone: Vec<PathBuf>,
    pub gone_under: Vec<PathBuf>,
}

struct Pending {
    path: PathBuf,
    kind: Kind,
    /// First heard of, since the last verdict.
    born: Instant,
    due: Instant,
    /// Size and mtime at the last check.
    seen: Option<(u64, u64)>,
    busy_wait: Duration,
}

pub struct Settler {
    pending: HashMap<String, Pending>,
    outcomes: BTreeMap<String, Outcome>,
    walks: Vec<PathBuf>,
    next_batch: Option<Instant>,
}

fn key(path: &Path) -> String {
    super::folders::folder_key(&path.to_string_lossy())
}

impl Settler {
    pub fn new() -> Self {
        Self { pending: HashMap::new(), outcomes: BTreeMap::new(), walks: Vec::new(), next_batch: None }
    }

    /// Something happened to `path`: wait for quiet again (a verdict still queued is superseded).
    pub fn note(&mut self, path: PathBuf, kind: Kind, now: Instant) {
        let key = key(&path);
        self.outcomes.remove(&key);
        let born = self.pending.get(&key).map_or(now, |p| p.born);
        self.pending.insert(key, Pending { path, kind, born, due: now + QUIET, seen: None, busy_wait: RECHECK });
    }

    /// Drops everything waiting under a folder that went away.
    pub fn forget_under(&mut self, folder: &Path) {
        let folder = folder.to_string_lossy();
        self.pending.retain(|_, p| !super::folders::is_inside(&p.path.to_string_lossy(), &folder));
    }

    /// Files to check now, longest waiting first, at most [`MAX_PROBES`].
    pub fn due(&self, now: Instant) -> Vec<(PathBuf, Kind)> {
        let mut due: Vec<&Pending> = self.pending.values().filter(|p| p.due <= now).collect();
        due.sort_by_key(|p| p.due);
        due.into_iter().take(MAX_PROBES).map(|p| (p.path.clone(), p.kind)).collect()
    }

    pub fn report(&mut self, path: &Path, probe: Probe, now: Instant) {
        let key = key(path);
        let Some(entry) = self.pending.get_mut(&key) else { return };
        match (probe, entry.kind) {
            (Probe::Directory, _) => {
                self.pending.remove(&key);
                self.walks.push(path.to_path_buf());
            }
            (Probe::Missing, Kind::Clip) => {
                self.pending.remove(&key);
                self.settle(key, Outcome::Gone(path.to_path_buf()), now);
            }
            (Probe::Missing, Kind::Other) => {
                self.pending.remove(&key);
                self.settle(key, Outcome::GoneUnder(path.to_path_buf()), now);
            }
            (_, Kind::Other) => {
                self.pending.remove(&key);
            }
            (Probe::Busy, Kind::Clip) => {
                entry.seen = None;
                entry.due = now + entry.busy_wait;
                entry.busy_wait = (entry.busy_wait * 2).min(BUSY_MAX);
            }
            (Probe::Ready { bytes: 0, .. }, Kind::Clip) if now.duration_since(entry.born) < EMPTY_GRACE => {
                entry.seen = None;
                entry.due = now + BUSY_MAX;
            }
            (Probe::Ready { bytes, mtime }, Kind::Clip) => {
                if entry.seen == Some((bytes, mtime)) {
                    let done = entry.path.clone();
                    self.pending.remove(&key);
                    self.settle(key, Outcome::Ready(done, bytes, mtime), now);
                } else {
                    entry.seen = Some((bytes, mtime));
                    entry.due = now + RECHECK;
                }
            }
        }
    }

    fn settle(&mut self, key: String, outcome: Outcome, now: Instant) {
        self.outcomes.insert(key, outcome);
        self.next_batch.get_or_insert(now);
    }

    /// Folders that appeared: their clips are found by one walk of that folder only.
    pub fn take_walks(&mut self) -> Vec<PathBuf> {
        std::mem::take(&mut self.walks)
    }

    /// The results so far, when the last batch is at least [`BATCH_GAP`] old.
    pub fn take_batch(&mut self, now: Instant) -> Option<Batch> {
        if self.outcomes.is_empty() || self.next_batch.is_some_and(|at| now < at) {
            return None;
        }
        self.next_batch = Some(now + BATCH_GAP);
        let mut batch = Batch::default();
        for outcome in std::mem::take(&mut self.outcomes).into_values() {
            match outcome {
                Outcome::Ready(path, bytes, mtime) => batch.ready.push((path, bytes, mtime)),
                Outcome::Gone(path) => batch.gone.push(path),
                Outcome::GoneUnder(path) => batch.gone_under.push(path),
            }
        }
        Some(batch)
    }

    /// How long the thread may sleep; `None` when nothing is waiting.
    pub fn next_wake(&self, now: Instant) -> Option<Duration> {
        let check = self.pending.values().map(|p| p.due).min();
        let batch = self.next_batch.filter(|_| !self.outcomes.is_empty());
        let at = check.into_iter().chain(batch).min()?;
        Some(at.saturating_duration_since(now).max(PASS_GAP))
    }
}
