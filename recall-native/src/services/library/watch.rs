//! Keeps the library in step with its clip folders while the app runs: a clip another program
//! adds, renames or removes shows up without a reopen.
//!
//! The clip drive is usually an SMR hard drive, so a change costs no folder rescan: the OS
//! names the file, `settle` waits until it is finished, and the UI gets the difference
//! ([`ClipChanges`]). Only an overflowed buffer or a folder that left and came back asks for a
//! re-list, at most every [`RESYNC_GAP`]. Thumbnails stay lazy: new tiles are thumbnailed when
//! they scroll into view, like any other. Checks are metadata only and skip the disk lane on
//! purpose: a lane can be held for minutes by an export. Nothing here writes to a clip drive.
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, RecvTimeoutError, Sender},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

use super::{
    folders::{self, FoundClip},
    settle::{Batch, Kind, Probe, Settler},
    watch_win::{self, Action, Raw, Stop},
};
use crate::{backend::ClipEntry, services::storage};

const RESYNC_GAP: Duration = Duration::from_secs(10);
/// How long `set_roots` waits for new folders to be armed (a sleeping drive spins up meanwhile).
const ARM_WAIT: Duration = Duration::from_secs(2);
/// How long `stop` waits for a thread. One stuck on a dead network path is left to process exit.
const JOIN_WAIT: Duration = Duration::from_secs(1);

/// One folder to watch: the recordings folder lists flat, imported ones with subfolders.
#[derive(Debug, Clone, PartialEq)]
pub struct WatchRoot {
    pub path: PathBuf,
    pub recursive: bool,
}

impl WatchRoot {
    fn key(&self) -> String {
        format!("{}|{}", folders::folder_key(&self.path.to_string_lossy()), self.recursive)
    }
}

/// A finished clip, ready to be a tile.
#[derive(Debug, Clone, PartialEq)]
pub struct AddedClip {
    pub entry: ClipEntry,
    /// An edit sidecar sits beside it.
    pub edited: bool,
}

/// What changed on disk since the last batch.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct ClipChanges {
    /// New clips, or clips whose size or mtime changed.
    pub added: Vec<AddedClip>,
    pub gone: Vec<String>,
    /// Paths that left: any clip listed under one left with it.
    pub gone_under: Vec<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum LibraryChange {
    Clips(ClipChanges),
    /// The folders changed in a way only a listing can tell.
    Resync,
    /// The set of watched folders changed: the UI's replay log no longer applies.
    RootsChanged,
    FolderLost(String),
    FolderBack(String),
}

type Sink = Box<dyn Fn(LibraryChange) + Send>;

struct RootThread {
    stop: Arc<Stop>,
    thread: JoinHandle<()>,
}

pub struct ClipWatcher {
    tx: Sender<Raw>,
    roots: Mutex<HashMap<String, RootThread>>,
    scope: Arc<Mutex<Vec<WatchRoot>>>,
    settle: Mutex<Option<JoinHandle<()>>>,
    stopped: AtomicBool,
}

impl ClipWatcher {
    /// Starts the settle thread. No folder is watched until [`Self::set_roots`].
    pub fn start(sink: impl Fn(LibraryChange) + Send + 'static) -> Self {
        let (tx, rx) = mpsc::channel();
        let scope = Arc::new(Mutex::new(Vec::new()));
        let settle = Settle { rx, scope: scope.clone(), sink: Box::new(sink), settler: Settler::new(), resync_at: None, last_resync: None };
        let settle = thread::Builder::new().name("recall-watch-settle".into()).spawn(move || settle.run()).ok();
        Self { tx, roots: Mutex::new(HashMap::new()), scope, settle: Mutex::new(settle), stopped: AtomicBool::new(false) }
    }

    /// Makes `wanted` the watched folders. Returns once each new folder is armed, so a listing
    /// taken after this call misses nothing (late changes arrive as [`ClipChanges`]).
    pub fn set_roots(&self, wanted: Vec<WatchRoot>) {
        if self.stopped.load(Ordering::Acquire) {
            return;
        }
        let mut running = self.roots.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let keys: Vec<String> = wanted.iter().map(WatchRoot::key).collect();
        let stale: Vec<String> = running.keys().filter(|key| !keys.contains(key)).cloned().collect();
        let fresh: Vec<&WatchRoot> = wanted.iter().filter(|root| !running.contains_key(&root.key())).collect();
        if stale.is_empty() && fresh.is_empty() {
            return;
        }
        for key in stale {
            if let Some(root) = running.remove(&key) {
                root.stop.set();
                join_bounded(root.thread);
            }
        }
        *self.scope.lock().unwrap_or_else(|poisoned| poisoned.into_inner()) = wanted.clone();
        let (armed_tx, armed_rx) = mpsc::channel();
        let mut started = 0;
        for root in fresh {
            if let Some(thread) = spawn_root(root, &self.tx, &armed_tx) {
                running.insert(root.key(), thread);
                started += 1;
            }
        }
        let deadline = Instant::now() + ARM_WAIT;
        for _ in 0..started {
            if armed_rx.recv_timeout(deadline.saturating_duration_since(Instant::now())).is_err() {
                break;
            }
        }
        let _ = self.tx.send(Raw::RootsChanged);
    }

    /// Stops and joins every thread. Safe to call twice.
    pub fn stop(&self) {
        self.stopped.store(true, Ordering::Release);
        let mut running = self.roots.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let roots: Vec<RootThread> = running.drain().map(|(_, root)| root).collect();
        roots.iter().for_each(|root| root.stop.set());
        roots.into_iter().for_each(|root| join_bounded(root.thread));
        let _ = self.tx.send(Raw::Stop);
        if let Some(settle) = self.settle.lock().ok().and_then(|mut settle| settle.take()) {
            join_bounded(settle);
        }
    }
}

impl Drop for ClipWatcher {
    fn drop(&mut self) {
        self.stop();
    }
}

fn spawn_root(root: &WatchRoot, tx: &Sender<Raw>, armed: &Sender<()>) -> Option<RootThread> {
    let stop = Arc::new(Stop::new()?);
    let (root, thread_stop, tx, armed) = (root.clone(), stop.clone(), tx.clone(), armed.clone());
    let thread = thread::Builder::new().name("recall-watch".into()).spawn(move || watch_win::watch_root(root, thread_stop, tx, armed)).ok()?;
    Some(RootThread { stop, thread })
}

fn join_bounded(thread: JoinHandle<()>) {
    let deadline = Instant::now() + JOIN_WAIT;
    while !thread.is_finished() && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(5));
    }
    if thread.is_finished() {
        let _ = thread.join();
    }
}

/// Stats a file; a read open with writers shut out tells whether a program still writes it.
pub(super) fn probe(path: &Path, kind: Kind) -> Probe {
    use std::{fs::OpenOptions, io::ErrorKind, os::windows::fs::OpenOptionsExt};
    const FILE_SHARE_READ: u32 = 1;
    const FILE_SHARE_DELETE: u32 = 4;
    let metadata = match path.metadata() {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == ErrorKind::NotFound => return Probe::Missing,
        Err(_) => return Probe::Busy,
    };
    if metadata.is_dir() {
        return Probe::Directory;
    }
    let ready = Probe::Ready { bytes: metadata.len(), mtime: storage::mtime_secs(&metadata) };
    if kind == Kind::Other {
        return ready;
    }
    match OpenOptions::new().read(true).share_mode(FILE_SHARE_READ | FILE_SHARE_DELETE).open(path) {
        Ok(_) => ready,
        Err(error) if error.kind() == ErrorKind::NotFound => Probe::Missing,
        Err(_) => Probe::Busy,
    }
}

/// The listing's view of a clip at `path`: the `folder` it would list it with, or `None` when
/// no watched folder lists it. Roots are in listing order, so the first one wins as it does there.
pub(super) fn covering_folder(roots: &[WatchRoot], path: &Path, skip: &[PathBuf]) -> Option<Option<String>> {
    roots.iter().find_map(|root| {
        if !root.recursive {
            let folder = path.parent()?.to_string_lossy().into_owned();
            return (folders::folder_key(&folder) == folders::folder_key(&root.path.to_string_lossy())).then_some(None);
        }
        folders::scan_covers(&root.path, path, skip).then(|| path.parent()?.file_name().map(|n| n.to_string_lossy().into_owned()))
    })
}

struct Settle {
    rx: Receiver<Raw>,
    scope: Arc<Mutex<Vec<WatchRoot>>>,
    sink: Sink,
    settler: Settler,
    resync_at: Option<Instant>,
    last_resync: Option<Instant>,
}

impl Settle {
    fn run(mut self) {
        loop {
            let now = Instant::now();
            let resync = self.resync_at.map(|at| at.saturating_duration_since(now));
            let wait = self.settler.next_wake(now).into_iter().chain(resync).min();
            let first = match wait {
                Some(wait) => self.rx.recv_timeout(wait),
                None => self.rx.recv().map_err(|_| RecvTimeoutError::Disconnected),
            };
            match first {
                Ok(raw) => {
                    if !self.absorb(raw) {
                        return;
                    }
                    while let Ok(more) = self.rx.try_recv() {
                        if !self.absorb(more) {
                            return;
                        }
                    }
                }
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => return,
            }
            self.pass(Instant::now());
        }
    }

    /// False on `Stop`.
    fn absorb(&mut self, raw: Raw) -> bool {
        let now = Instant::now();
        match raw {
            Raw::Change { root, recursive, path, action } => {
                if folders::is_clip(&path) {
                    self.settler.note(path, Kind::Clip, now);
                } else if recursive && action != Action::Modified && folders::scan_reaches(&root, &path.join("x")) {
                    self.settler.note(path, Kind::Other, now);
                }
            }
            Raw::Overflow(root) => {
                folders::forget_scan(&root);
                self.ask_resync(now);
            }
            Raw::Lost(root) => {
                self.settler.forget_under(&root);
                folders::forget_scan(&root);
                (self.sink)(LibraryChange::FolderLost(root.to_string_lossy().into_owned()));
                self.ask_resync(now);
            }
            Raw::Back(root) => {
                folders::forget_scan(&root);
                (self.sink)(LibraryChange::FolderBack(root.to_string_lossy().into_owned()));
                self.ask_resync(now);
            }
            Raw::RootsChanged => (self.sink)(LibraryChange::RootsChanged),
            Raw::Stop => return false,
        }
        true
    }

    fn ask_resync(&mut self, now: Instant) {
        let earliest = self.last_resync.map_or(now, |last| (last + RESYNC_GAP).max(now));
        self.resync_at.get_or_insert(earliest);
    }

    fn pass(&mut self, now: Instant) {
        for (path, kind) in self.settler.due(now) {
            let result = probe(&path, kind);
            self.settler.report(&path, result, now);
        }
        for dir in self.settler.take_walks() {
            self.walk(&dir, now);
        }
        if let Some(batch) = self.settler.take_batch(now) {
            self.emit(batch);
        }
        if self.resync_at.is_some_and(|at| at <= now) {
            self.resync_at = None;
            self.last_resync = Some(now);
            (self.sink)(LibraryChange::Resync);
        }
    }

    /// A folder arrived whole (a move sends one event for it): one walk of that folder only.
    fn walk(&mut self, dir: &Path, now: Instant) {
        let root = self.scope.lock().ok().and_then(|roots| roots.iter().find(|r| r.recursive && folders::is_inside(&dir.to_string_lossy(), &r.path.to_string_lossy())).cloned());
        let Some(root) = root else { return };
        for clip in folders::scan_subfolder(&root.path, dir, &[storage::cache_root()]) {
            self.settler.note(clip.path, Kind::Clip, now);
        }
    }

    fn emit(&self, batch: Batch) {
        let roots = self.scope.lock().map(|roots| roots.clone()).unwrap_or_default();
        let skip = [storage::cache_root()];
        let mut found = Vec::new();
        for (path, bytes, mtime) in batch.ready {
            if let Some(folder) = covering_folder(&roots, &path, &skip) {
                found.push(FoundClip { path, bytes, mtime, folder });
            }
        }
        let text = |paths: Vec<PathBuf>| paths.into_iter().map(|p| p.to_string_lossy().into_owned()).collect::<Vec<_>>();
        let (gone, gone_under) = (text(batch.gone), text(batch.gone_under));
        folders::patch_scans(&found, &gone, &gone_under);
        let recordings = roots.iter().find(|r| !r.recursive).map(|r| r.path.clone()).unwrap_or_default();
        let imported: Vec<String> = roots.iter().filter(|r| r.recursive).map(|r| r.path.to_string_lossy().into_owned()).collect();
        let former = folders::Folders::load().former_recordings;
        let found = super::import_check::listable(found, &recordings, &imported, &former);
        let added = found.into_iter().map(added_clip).collect();
        (self.sink)(LibraryChange::Clips(ClipChanges { added, gone, gone_under }));
    }
}

fn added_clip(clip: FoundClip) -> AddedClip {
    let edited = super::sidecar_candidates(&clip.path).iter().any(|candidate| candidate.exists());
    let name = clip.path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let path = clip.path.to_string_lossy().into_owned();
    AddedClip { entry: ClipEntry { name, path, bytes: clip.bytes, mtime: clip.mtime, thumb: None, folder: clip.folder }, edited }
}
