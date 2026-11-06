//! The `recall-storage` schedule and worker thread: storage-cap runs
//! (`record_storage.rs`) and the free-space check (`record_free_space.rs`).
//! Runs fall due 5 s after a save (bursts coalesce), hourly, on demand, and
//! one free-space-only check [`START_CHECK`] after start. Every run reads
//! free space; the cap part runs only when `storage.cap_gb` is set.

use std::path::{Path, PathBuf};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::control::PipeHub;
use crate::log;
use crate::record_free_space::{self as free_space, FreeCheck, FreeFn, FreeWarner, Resolve};
use crate::record_storage::{cleaned_event, plan_dir, preview_event, run_once, warning_event, Warner, AFTER_SAVE, HOURLY};

/// The first free-space check after start (the engine is up by then).
pub const START_CHECK: Duration = Duration::from_secs(60);

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Job {
    pub dir: PathBuf,
    /// 0 = no cap run.
    pub cap_gb: u32,
    /// The scheduled free-space-only check: it never replaces a queued cap run.
    pub free_only: bool,
    /// `Some(client)`: a dry run answered to that client; nothing moves.
    pub preview_for: Option<u64>,
    /// Free-space check of the clip and staging drives.
    pub free: Option<FreeCheck>,
}

/// Schedules runs (after saves, hourly, on demand) onto one worker thread.
pub struct Janitor {
    tx: Sender<Job>,
    after_save: Option<Instant>,
    next_hourly: Instant,
    /// The one free-space check after start ([`START_CHECK`]).
    free_soon: Option<Instant>,
    /// Replay buffer bytes (from the config, refreshed every tick).
    buffer_bytes: u64,
    staging: Option<PathBuf>,
    /// The storage thread's volume resolver (it may block, so the pump never calls it).
    resolve: Resolve,
}

impl Janitor {
    pub fn start(hub: Arc<PipeHub>) -> Self {
        let (tx, rx) = channel::<Job>();
        let janitor = Janitor::with_sender(tx, free_space::engine_staging_dir(), Instant::now(), crate::win_volume::volume_of);
        let resolve = janitor.resolve;
        let _ = std::thread::Builder::new()
            .name("recall-storage".into())
            .spawn(move || worker(&hub, rx, free_space::free_bytes, resolve));
        janitor
    }

    pub(crate) fn with_sender(tx: Sender<Job>, staging: Option<PathBuf>, now: Instant, resolve: Resolve) -> Self {
        let free_soon = Some(now + START_CHECK);
        Janitor { tx, after_save: None, next_hourly: now + HOURLY, free_soon, buffer_bytes: 0, staging, resolve }
    }

    pub fn saved(&mut self, now: Instant) {
        self.after_save = Some(now + AFTER_SAVE);
    }

    pub fn next_deadline(&self) -> Option<Instant> {
        self.after_save.into_iter().chain(self.free_soon).min()
    }

    /// Plain paths only: the storage thread resolves their volumes (that can block).
    fn free_check(&self, dir: &str) -> FreeCheck {
        FreeCheck::new(Path::new(dir), self.staging.as_deref(), self.buffer_bytes)
    }

    /// Run now (cap changed / pipe `storage_clean` / schedule); cap 0 = free space only.
    pub fn run_now(&mut self, dir: &str, cap_gb: u32, now: Instant) {
        self.after_save = None;
        self.free_soon = None;
        self.next_hourly = now + HOURLY;
        let _ = self.tx.send(Job { dir: PathBuf::from(dir), cap_gb, free_only: false, preview_for: None, free: Some(self.free_check(dir)) });
    }

    /// Plan `cap_gb` without moving anything; the answer goes to `client` only.
    pub fn preview(&mut self, dir: &str, cap_gb: u32, client: u64) {
        let _ = self.tx.send(Job { dir: PathBuf::from(dir), cap_gb, free_only: false, preview_for: Some(client), free: None });
    }

    /// Every pump tick. `buffer_bytes`: the replay buffer at the configured bitrate.
    pub fn tick(&mut self, dir: &str, cap_gb: u32, buffer_bytes: u64, now: Instant) {
        self.buffer_bytes = buffer_bytes;
        if self.after_save.is_some_and(|t| now >= t) || now >= self.next_hourly {
            self.run_now(dir, cap_gb, now);
        } else if self.free_soon.is_some_and(|t| now >= t) {
            self.free_soon = None;
            let job = Job { dir: PathBuf::from(dir), cap_gb: 0, free_only: true, preview_for: None, free: Some(self.free_check(dir)) };
            let _ = self.tx.send(job);
        }
    }
}

/// Queued runs coalesce into the newest; the scheduled free-only check never
/// drops a queued cap run (but a newer cap of 0, the limit turned off, does).
pub(crate) fn coalesce(prev: Option<Job>, job: Job) -> Job {
    match prev {
        Some(p) if job.free_only => Job { free: job.free.or(p.free), ..p },
        _ => job,
    }
}

/// The `disk_low` events this check sends.
/// Storage thread: resolves the folders' volumes (may block) and reads free space.
pub(crate) fn free_warnings(warner: &mut FreeWarner, check: &FreeCheck, free: FreeFn, resolve: Resolve) -> Vec<String> {
    let drives = free_space::resolve_drives(&check.dirs, resolve);
    warner
        .update(&drives, check.threshold, free)
        .into_iter()
        .map(|(drive, have)| {
            log!("storage: {drive} has {have} bytes free, below {} (2 x the replay buffer, at least 5 GB)", check.threshold);
            free_space::free_event(&drive, have, check.threshold)
        })
        .collect()
}

fn worker(hub: &PipeHub, rx: Receiver<Job>, free: FreeFn, resolve: Resolve) {
    background_io();
    let (mut warner, mut low) = (Warner::default(), FreeWarner::default());
    while let Ok(first) = rx.recv() {
        // Previews are answered in order; queued runs coalesce.
        let mut run = None;
        for job in std::iter::once(first).chain(std::iter::from_fn(|| rx.try_recv().ok())) {
            match job.preview_for {
                Some(client) => match plan_dir(&job.dir, job.cap_gb) {
                    Ok((plan, _)) => hub.event_to(client, "session", &preview_event(job.cap_gb, &plan)),
                    Err(e) => hub.event_to(client, "session", &crate::record_status::error_event(&format!("storage preview: {e}"))),
                },
                None => run = Some(coalesce(run, job)),
            }
        }
        let Some(job) = run else { continue };
        if let Some(check) = &job.free {
            for event in free_warnings(&mut low, check, free, resolve) {
                hub.event("session", &event);
            }
        }
        if job.cap_gb > 0 {
            cap_run(hub, &job, &mut warner);
        }
    }
}

fn cap_run(hub: &PipeHub, job: &Job, warner: &mut Warner) {
    match run_once(&job.dir, job.cap_gb) {
        Ok(o) => {
            if o.removed > 0 {
                log!("storage: cap {} GiB -> {} clip(s) to the Recycle Bin, {} bytes freed", job.cap_gb, o.removed, o.freed);
                hub.event("session", &cleaned_event(&o));
            } else {
                log!("storage: {} uses {} of its {} GiB cap", job.dir.display(), o.used, job.cap_gb);
            }
            if let Some(level) = warner.update(o.used, o.cap, o.stuck, o.removed > 0) {
                log!("storage: {level}% warning ({} of {} bytes)", o.used, o.cap);
                hub.event("session", &warning_event(level, &o));
            }
        }
        Err(e) => {
            log!("storage: clean-up skipped -- {e}");
            hub.event("session", &crate::record_status::error_event(&format!("storage clean-up: {e}")));
        }
    }
}

/// Background I/O + CPU priority for this thread (the HDD stays responsive).
fn background_io() {
    use windows::Win32::System::Threading::{GetCurrentThread, SetThreadPriority, THREAD_MODE_BACKGROUND_BEGIN};
    // SAFETY: the current-thread pseudo-handle needs no closing.
    let _ = unsafe { SetThreadPriority(GetCurrentThread(), THREAD_MODE_BACKGROUND_BEGIN) };
}

#[cfg(test)]
#[path = "record_janitor_tests.rs"]
mod tests;
