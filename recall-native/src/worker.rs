//! Background threads. The UI thread never blocks on disk, ffmpeg or the engine.
//! `io`: list / save / flush / export / delete / storage (serial, in order). Each clip open
//! runs on its own thread (`clip_open.rs`).
//! `assets`: thumbnails, durations, video copies. `scrub`: hover-scrub frames.
//!
//! Work that reads an HDD is routed onto that disk's I/O lane (`services::storage`): one
//! reader per physical disk at very low I/O priority, hover-scrub ahead of visible
//! thumbnails ahead of idle prefetch. SSD work runs directly on these threads.
use std::{
    collections::HashSet,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
        mpsc::{self, Receiver, Sender},
    },
    thread,
};

use crate::{
    backend::{self, ClipEntry, ExportSummary, PreparedClip, StorageReport},
    clip_open::StreamedClip,
    edit_model::{ClipInfo, EditPlan, ExportSettings},
    services::{
        library::{
            self, MetaStore,
            watch::{ClipWatcher, LibraryChange},
        },
        storage::{self, CacheKind, PrefetchClip, PrefetchEvent, Priority},
    },
};

/// A library listing with what the UI thread must not ask the drive for itself.
pub struct Listing {
    pub entries: Vec<ClipEntry>,
    /// Lowercase paths of the edit sidecars beside the clips.
    pub sidecars: HashSet<String>,
    pub drive: Option<(u64, u64)>,
}

pub enum Job {
    List,
    /// Autosave: working copy on the SSD (`Done::Saved`).
    SaveProject(ClipInfo, EditPlan, u32, ExportSettings),
    /// Leave / close / export: the sidecar beside the clip, only if it changed (`Done::Flushed`).
    FlushProject(ClipInfo, EditPlan, u32, ExportSettings, bool),
    Export(ClipInfo, EditPlan, u32, ExportSettings),
    Delete(Vec<PathBuf>),
    /// Free space of the recordings drive (`Done::Drive`).
    DriveSpace,
    /// Drive types + cache usage for Settings (`Done::Storage`).
    StorageReport,
    /// Settings "clear" for one tier, then a fresh report (`Done::Storage`, with a notice).
    /// (Whole-cache clear / budget: `storage::clear()` / `storage::set_budget_bytes()`.)
    ClearCache(CacheKind),
}

pub enum AssetJob {
    Thumb(String),
    Duration(String),
    /// Copy the clip to the SSD video tier (`Done::VideoCached`).
    CacheVideo(String),
    /// A 720p H.264 preview copy for a clip the player cannot open (`Done::Proxy`).
    Proxy(String),
}

pub enum Done {
    Clips(Result<Listing, String>),
    /// (free, total) bytes of the recordings drive.
    Drive(Option<(u64, u64)>),
    /// SSD cache totals for the sidebar (`Workers::ssd_usage`).
    Ssd(storage::SsdUsage),
    /// The clip can play while it loads (`clip_open.rs`); `Prepared` follows when it has loaded.
    Streaming(u64, Box<StreamedClip>),
    /// What an open without streaming is doing now.
    OpenStep(u64, &'static str),
    Prepared(u64, Result<PreparedClip, String>),
    Saved(Result<String, String>),
    Exported(Result<ExportSummary, String>),
    /// The clips asked for: (moved to the Recycle Bin, removed from Recall only).
    Deleted(Vec<PathBuf>, Result<(usize, usize), String>),
    Thumb(String, Result<String, String>),
    Duration(String, Option<f64>),
    Scrub(String, Result<Vec<String>, String>),
    /// (clip path, SSD copy path — or the clip itself when it already lives on an SSD).
    VideoCached(String, Result<String, String>),
    /// Sidecar path when current; "" when there was nothing to write.
    Flushed(Result<backend::Flushed, String>),
    /// (clip path, preview copy path).
    Proxy(String, Result<String, String>),
    Storage(StorageReport),
    /// The clip folders changed on disk (`services::library::watch`).
    Library(LibraryChange),
}

pub struct Workers {
    jobs: Sender<Job>,
    assets: Sender<AssetJob>,
    scrub: Sender<String>,
    open_tx: Sender<Done>,
    pub done: Receiver<Done>,
}

impl Workers {
    pub fn spawn() -> Self {
        let (done_tx, done) = mpsc::channel::<Done>();
        let (jobs, job_rx) = mpsc::channel::<Job>();
        let (assets, asset_rx) = mpsc::channel::<AssetJob>();
        let (scrub, scrub_rx) = mpsc::channel::<String>();

        let open_tx = done_tx.clone();
        let tx = done_tx.clone();
        library::import_check::set_sink(Box::new(move |paths| {
            let gone = paths.iter().map(|p| p.to_string_lossy().into_owned()).collect();
            let _ = tx.send(Done::Library(library::watch::LibraryChange::Clips(library::watch::ClipChanges { added: vec![], gone, gone_under: vec![] })));
        }));
        let tx = done_tx.clone();
        let watcher = ClipWatcher::start(move |change| {
            let _ = tx.send(Done::Library(change));
        });

        let tx = done_tx.clone();
        spawn_named("recall-io", move || {
            while let Ok(job) = job_rx.recv() {
                if tx.send(run_job(job, &tx, &watcher)).is_err() {
                    break;
                }
            }
        });

        let tx = done_tx.clone();
        spawn_named("recall-assets", move || {
            while let Ok(job) = asset_rx.recv() {
                route_asset(job, &tx);
            }
        });

        spawn_named("recall-scrub", move || {
            while let Ok(mut path) = scrub_rx.recv() {
                // The latest hover is urgent; earlier ones still get answered, at low priority.
                while let Ok(newer) = scrub_rx.try_recv() {
                    route_scrub(path, Priority::Background, &done_tx);
                    path = newer;
                }
                route_scrub(path, Priority::Interactive, &done_tx);
            }
        });

        Self { jobs, assets, scrub, done, open_tx }
    }

    /// Opens a clip on a thread of its own: a running export or listing never holds it up.
    pub fn open_clip(&self, generation: u64, path: String) {
        crate::clip_open::note_open(generation);
        let tx = self.open_tx.clone();
        spawn_named("recall-clip-open", move || crate::clip_open::run(generation, path, &tx));
    }

    /// Walks the SSD cache on a thread of its own (`Done::Ssd`): an export on the io worker never holds it up.
    pub fn ssd_usage(&self) {
        let tx = self.open_tx.clone();
        spawn_named("recall-ssd-usage", move || {
            let _ = tx.send(Done::Ssd(storage::ssd_usage(&backend::library_dir())));
        });
    }

    pub fn job(&self, job: Job) {
        let _ = self.jobs.send(job);
    }

    pub fn asset(&self, job: AssetJob) {
        let _ = self.assets.send(job);
    }

    pub fn scrub(&self, path: String) {
        let _ = self.scrub.send(path);
    }
}

fn run_job(job: Job, tx: &Sender<Done>, watcher: &ClipWatcher) -> Done {
    match job {
        Job::List => {
            // Every folder change ends in a listing. Armed first, so nothing falls between.
            watcher.set_roots(backend::clip_roots());
            let result = backend::list_clips_checked().map(|(entries, complete)| {
                prefetch(&entries, complete, tx.clone());
                Listing { sidecars: library::sidecar_index(&entries), drive: library::drive_space(&backend::library_dir()), entries }
            });
            Done::Clips(result)
        }
        Job::DriveSpace => Done::Drive(library::drive_space(&backend::library_dir())),
        Job::SaveProject(info, plan, fps, export) => Done::Saved(backend::save_project(&info, &plan, fps, &export)),
        Job::FlushProject(info, plan, fps, export, ours) => Done::Flushed(backend::flush_project(&info, &plan, fps, &export, ours)),
        Job::Export(info, plan, fps, export) => Done::Exported(crate::services::pre_encode::export(&info, &plan, fps, &export)),
        Job::Delete(paths) => {
            let result = library::delete_or_hide(&paths);
            library::folders::forget_scans();
            Done::Deleted(paths, result)
        }
        Job::StorageReport => Done::Storage(backend::storage_report(None)),
        Job::ClearCache(kind) => {
            let notice = match storage::clear_kind(kind) {
                Ok(freed) => format!("Cleared {} of {}.", library::format_bytes(freed), kind.label().to_lowercase()),
                Err(error) => format!("Couldn't clear {}: {error}", kind.label().to_lowercase()),
            };
            Done::Storage(backend::storage_report(Some(notice)))
        }
    }
}

/// Thumbnails / durations are queued on the clip's disk lane at `Visible` priority (the
/// assets thread never blocks on an HDD); video copies at `Interactive`.
fn route_asset(job: AssetJob, tx: &Sender<Done>) {
    let tx = tx.clone();
    match job {
        AssetJob::Thumb(path) => storage::submit_io(&PathBuf::from(&path), Priority::Visible, move || {
            let result = backend::thumbnail(&path);
            let _ = tx.send(Done::Thumb(path, result));
        }),
        AssetJob::Duration(path) => storage::submit_io(&PathBuf::from(&path), Priority::Visible, move || {
            let secs = library::mp4_duration(Path::new(&path));
            let _ = tx.send(Done::Duration(path, secs));
        }),
        AssetJob::Proxy(path) => spawn_named("recall-preview-copy", move || {
            let result = storage::proxy::preview_proxy(&path);
            let _ = tx.send(Done::Proxy(path, result));
        }),
        AssetJob::CacheVideo(path) => {
            // Runs on its own short-lived thread: cache_video waits on the lane, and a copy
            // takes seconds; the assets thread keeps routing meanwhile.
            spawn_named("recall-video-copy", move || {
                let result = storage::cache_video(&path);
                let _ = tx.send(Done::VideoCached(path, result));
            });
        }
    }
}

static SCRUB_GENERATION: AtomicU64 = AtomicU64::new(0);

/// Cached frames answer at once; misses go to the disk lane. An urgent request that is no
/// longer the latest hover when its turn comes is re-queued behind visible work.
fn route_scrub(path: String, priority: Priority, tx: &Sender<Done>) {
    if let Some(frames) = library::cached_scrub_frames(&path) {
        let _ = tx.send(Done::Scrub(path, Ok(frames)));
        return;
    }
    let generation = SCRUB_GENERATION.fetch_add(1, Ordering::Relaxed) + 1;
    let tx = tx.clone();
    let lane_path = PathBuf::from(&path);
    storage::submit_io(&lane_path.clone(), priority, move || {
        if priority == Priority::Interactive && SCRUB_GENERATION.load(Ordering::Relaxed) != generation {
            let tx = tx.clone();
            let path = path.clone();
            storage::submit_io(&lane_path, Priority::Background, move || send_scrub(path, &tx));
            return;
        }
        send_scrub(path, &tx);
    });
}

fn send_scrub(path: String, tx: &Sender<Done>) {
    let result = library::scrub_frames(&path);
    let _ = tx.send(Done::Scrub(path, result));
}

/// Smart-cache plan for the listed clips. Durations already in the metadata store are
/// skipped; results arrive as ordinary `Done::Thumb` / `Duration` / `Scrub`.
fn prefetch(entries: &[ClipEntry], complete: bool, tx: Sender<Done>) {
    let store = MetaStore::load();
    let records = library::build_records(entries, &store);
    let clips = entries
        .iter()
        .zip(records)
        .map(|(entry, record)| PrefetchClip {
            path: entry.path.clone(),
            name: entry.name.clone(),
            bytes: entry.bytes,
            mtime_secs: entry.mtime,
            has_thumb: entry.thumb.is_some(),
            duration: record.duration,
            game_key: record.game_key,
            favourite: record.favourite,
            edited: record.edited,
        })
        .collect();
    storage::start_prefetch(
        clips,
        complete,
        Arc::new(move |event| {
            let done = match event {
                PrefetchEvent::Thumb(path, result) => Done::Thumb(path, result),
                PrefetchEvent::Duration(path, secs) => Done::Duration(path, secs),
                PrefetchEvent::Scrub(path, result) => Done::Scrub(path, result),
            };
            let _ = tx.send(done);
        }),
        backend::prefetch_editor_media,
    );
}

fn spawn_named(name: &str, body: impl FnOnce() + Send + 'static) {
    if let Err(error) = thread::Builder::new().name(name.into()).spawn(body) {
        eprintln!("start {name}: {error}");
    }
}
