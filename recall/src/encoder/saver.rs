//! `recall-saver`: the single background consumer that turns a `SaveJob` into
//! a finished clip, so muxing never blocks the `recall-encoder` thread.
//!
//! The encoder thread builds a self-contained job in milliseconds (shared
//! packet window, owned codec parameters, PCM already snapshotted) and
//! `try_send`s it here. Jobs run FIFO, one at a time: still-encode if needed,
//! AAC encode, mux into `<target>.part`, rename, then the `Saved` event and
//! gallery hookup the encoder used to emit inline.
//!
//! The target is the clip's final path, or, when the clip folder is on another
//! volume than the SSD staging folder, `<staging>\<final name>` (`staging.rs`):
//! `Saved` then reports the FINAL path with `staged: true` and the
//! `recall-mover` thread (`mover.rs`) moves the clip there in the background.

use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering::Relaxed};
use std::sync::mpsc::{
    channel, sync_channel, Receiver, RecvTimeoutError, SyncSender, TrySendError,
};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

pub(crate) use super::events::Events;
use super::events::SavedReport;
use super::mover::{self, MoverControl};
use super::mux::OwnedParams;
use super::staging::{self, Stage};
use super::StillSpec;
use crate::audio::AudioPcm;
use crate::naming::part_of;
use crate::ring::EncodedPacket;

/// Jobs that may wait behind the one being written.
pub(super) const QUEUE_DEPTH: usize = 2;
pub(super) const QUEUE_FULL: &str = "previous save still writing";
pub(super) const ABORTED: &str = "save aborted: engine quitting";
const QUIT_BEFORE_START: &str = "engine quit before the save started";

/// Newest raw capture frame: (width, height, tightly packed RGBA).
pub(crate) type RawFrame = (u32, u32, Arc<Vec<u8>>);

pub(crate) enum VideoSrc {
    /// Ring window, stream-copied with the live encoder's parameters.
    Window {
        packets: Vec<EncodedPacket>,
        params: OwnedParams,
    },
    /// Quiet tail: the saver encodes this frame as a fresh keyframe-only still
    /// with the live encoder's codec and quality.
    Still { frame: RawFrame, spec: StillSpec },
}

/// Everything one save needs, detached from the live encoder and rings.
pub(crate) struct SaveJob {
    pub path: String,
    pub seconds: u32,
    /// When the save was requested; `elapsed_ms` is measured from here.
    pub requested: Instant,
    /// Clip duration the audio PCM was snapshotted for.
    pub secs: f64,
    /// Capture-clock end of the clip's video (None without a capture clock):
    /// bookmarks are placed against it (`bookmarks::take_saved`).
    pub video_end_us: Option<u64>,
    pub video: VideoSrc,
    pub backend: &'static str,
    pub audio: Vec<AudioPcm>,
    pub thumb: Option<RawFrame>,
    pub timing: Timing,
}

/// Result of a successful mux: what the `Saved` event reports.
pub(crate) struct Muxed {
    pub frames: u64,
    pub secs: f32,
}

/// Writes `job` to the given `.part` path (the saver renames it afterwards).
pub(crate) type MuxFn =
    Box<dyn FnMut(&Path, &SaveJob, &AtomicBool, &mut Timing) -> Result<Muxed, String> + Send>;
/// Post-save gallery hookup: (job, seconds, bytes, events).
pub(crate) type GalleryFn = Box<dyn FnMut(&SaveJob, f32, u64, &Events) + Send>;

/// Per-save phase timers, in milliseconds, logged as one `encoder_log` line.
#[derive(Clone, Copy, Default)]
pub(crate) struct Timing {
    /// Request -> the encoder thread picked it up.
    pub request_wait_ms: f64,
    pub window_ms: f64,
    pub snapshot_ms: f64,
    /// Whole save handling on the encoder thread (what can stall capture).
    pub encoder_blocked_ms: f64,
    pub queued_at: Option<Instant>,
    pub queue_wait_ms: f64,
    pub still_ms: f64,
    pub aac_ms: f64,
    /// CPU time of all audio encode threads together, and the encoder used ("mf" / "native").
    pub aac_cpu_ms: f64,
    pub aac_coder: &'static str,
    pub mux_ms: f64,
    pub trailer_ms: f64,
    pub rename_ms: f64,
    /// Request -> file renamed into place (the `elapsed_ms` of `Saved`).
    pub ready_ms: f64,
    pub gallery_ms: f64,
}

pub(crate) fn ms(d: Duration) -> f64 {
    d.as_secs_f64() * 1000.0
}

impl Timing {
    fn line(&self, outcome: &str, secs: f32, frames: u64, bytes: u64) -> String {
        format!(
            "save timing ({outcome}): secs={secs:.3} packets={frames} bytes={bytes} | \
             encoder thread: request_wait={:.1}ms window={:.1}ms snapshot={:.1}ms blocked={:.1}ms | \
             saver: queue_wait={:.1}ms still={:.1}ms aac={:.1}ms aac_cpu={:.1}ms aac_coder={} mux={:.1}ms trailer={:.1}ms \
             rename={:.1}ms ready={:.1}ms gallery={:.1}ms",
            self.request_wait_ms,
            self.window_ms,
            self.snapshot_ms,
            self.encoder_blocked_ms,
            self.queue_wait_ms,
            self.still_ms,
            self.aac_ms,
            self.aac_cpu_ms,
            self.aac_coder,
            self.mux_ms,
            self.trailer_ms,
            self.rename_ms,
            self.ready_ms,
            self.gallery_ms,
        )
    }
}

/// End of a rebased window in seconds (its clip length).
pub(crate) fn window_secs(packets: &[EncodedPacket]) -> f64 {
    packets
        .last()
        .map(|p| (p.frame_idx + p.duration_us) as f64 / 1_000_000.0)
        .unwrap_or(0.0)
}

/// Encoder-thread end of the saver queue.
pub(crate) struct SaverQueue {
    tx: SyncSender<SaveJob>,
    events: Events,
}

impl SaverQueue {
    pub fn events(&self) -> &Events {
        &self.events
    }

    /// Hand a job to the saver without blocking. A full queue fails this save
    /// immediately instead of stalling capture behind the writer.
    pub fn submit(&self, mut job: SaveJob) -> bool {
        job.timing.queued_at = Some(Instant::now());
        match self.tx.try_send(job) {
            Ok(()) => true,
            Err(TrySendError::Full(_)) => {
                self.events.save_failed(QUEUE_FULL);
                false
            }
            Err(TrySendError::Disconnected(_)) => {
                self.events.save_failed("save writer thread stopped");
                false
            }
        }
    }
}

/// How a bounded shutdown ended.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Shutdown {
    /// Every queued save finished (or there was none).
    Drained,
    /// The in-flight save was aborted at the deadline and cleaned up.
    Aborted,
    /// The saver did not stop in time; its `.part` removal was attempted.
    TimedOut,
}

/// Main-thread handle used to stop the saver (and its clip mover) at quit.
pub(crate) struct SaverControl {
    done: Receiver<()>,
    abort: Arc<AtomicBool>,
    current: Arc<Mutex<Option<PathBuf>>>,
    mover: Option<MoverControl>,
}

impl SaverControl {
    /// Wait for the saver to finish after its queue sender is dropped. Saves
    /// run normally for most of `budget`; for the last share (<= 500 ms) the
    /// in-flight mux is aborted so its `.part` is removed rather than orphaned.
    /// Then the clip mover stops (up to `mover::STOP_WAIT`): a move in flight
    /// is abandoned, its staged copy kept for the next start.
    pub fn shutdown(self, budget: Duration) -> Shutdown {
        let SaverControl { done, abort, current, mover } = self;
        let outcome = stop_saver(&done, &abort, &current, budget);
        if let Some(mover) = mover {
            mover.shutdown(mover::STOP_WAIT);
        }
        outcome
    }
}

fn stop_saver(
    done: &Receiver<()>,
    abort: &AtomicBool,
    current: &Mutex<Option<PathBuf>>,
    budget: Duration,
) -> Shutdown {
    let reserve = (budget / 2).min(Duration::from_millis(500));
    if wait_done(done, budget - reserve) {
        return Shutdown::Drained;
    }
    abort.store(true, Relaxed);
    if wait_done(done, reserve) {
        return Shutdown::Aborted;
    }
    if let Some(part) = current.lock().unwrap_or_else(|p| p.into_inner()).take() {
        let _ = std::fs::remove_file(part);
    }
    Shutdown::TimedOut
}

fn wait_done(done: &Receiver<()>, timeout: Duration) -> bool {
    !matches!(done.recv_timeout(timeout), Err(RecvTimeoutError::Timeout))
}

/// Spawn the production saver (real mux + gallery thumbnail + SSD staging).
pub(super) fn spawn_default(events: Events) -> (SaverQueue, SaverControl) {
    spawn_staged(
        events,
        Box::new(super::mux::produce),
        Box::new(super::mux::finish_gallery),
        Some(staging::Setup::from_env()),
    )
}

/// Spawn `recall-saver` with injectable mux/gallery stages, writing in place.
pub(crate) fn spawn(events: Events, mux: MuxFn, gallery: GalleryFn) -> (SaverQueue, SaverControl) {
    spawn_staged(events, mux, gallery, None)
}

/// [`spawn`] with optional SSD staging: leftovers are recovered and the clip
/// mover started before the saver takes its first job.
pub(crate) fn spawn_staged(
    events: Events,
    mux: MuxFn,
    gallery: GalleryFn,
    setup: Option<staging::Setup>,
) -> (SaverQueue, SaverControl) {
    let (stage, mover) = match setup {
        Some(setup) => staging::start(&events, setup),
        None => (None, None),
    };
    let (tx, rx) = sync_channel::<SaveJob>(QUEUE_DEPTH);
    let (done_tx, done_rx) = channel::<()>();
    let abort = Arc::new(AtomicBool::new(false));
    let current = Arc::new(Mutex::new(None));
    let worker = Worker {
        events: events.clone(),
        mux,
        gallery,
        abort: abort.clone(),
        current: current.clone(),
        stage,
    };
    let spawned = std::thread::Builder::new()
        .name("recall-saver".into())
        .spawn(move || {
            // Dropped when this thread ends: that disconnect is the done signal.
            let _done = done_tx;
            worker.run(rx);
        });
    if let Err(e) = spawned {
        events.log(&format!("saver thread failed to start: {e}"));
    }
    (
        SaverQueue { tx, events },
        SaverControl {
            done: done_rx,
            abort,
            current,
            mover,
        },
    )
}

struct Worker {
    events: Events,
    mux: MuxFn,
    gallery: GalleryFn,
    abort: Arc<AtomicBool>,
    current: Arc<Mutex<Option<PathBuf>>>,
    stage: Option<Stage>,
}

impl Worker {
    fn run(mut self, rx: Receiver<SaveJob>) {
        lower_thread_priority(&self.events);
        while let Ok(job) = rx.recv() {
            if self.abort.load(Relaxed) {
                self.events.save_failed(QUIT_BEFORE_START);
                continue;
            }
            let staged = self.stage.as_ref().and_then(|s| s.target(&job.path, &self.events));
            let target = staged.unwrap_or_else(|| PathBuf::from(&job.path));
            let part = part_of(&target);
            let outcome = catch_unwind(AssertUnwindSafe(|| self.process(&job, &target, &part)));
            if outcome.is_err() {
                // A panicking stage must not kill the saver or leave a half file.
                let _ = std::fs::remove_file(&part);
                self.set_current(None);
                self.events.save_failed("internal error while writing the clip");
            }
        }
    }

    fn set_current(&self, part: Option<PathBuf>) {
        *self.current.lock().unwrap_or_else(|p| p.into_inner()) = part;
    }

    /// Write `job` to `target` (its final path, or its staged file) via `part`.
    fn process(&mut self, job: &SaveJob, target: &Path, part: &Path) {
        let mut timing = job.timing;
        timing.queue_wait_ms = timing.queued_at.map(|t| ms(t.elapsed())).unwrap_or(0.0);
        self.set_current(Some(part.to_path_buf()));
        let result = write_clip_to(part, target, job, &mut *self.mux, &self.abort, &mut timing);
        self.set_current(None);
        match result {
            Ok(muxed) => {
                let bytes = std::fs::metadata(target).map(|m| m.len()).unwrap_or(0);
                let elapsed = job.requested.elapsed();
                timing.ready_ms = ms(elapsed);
                let staged = (target != Path::new(&job.path)).then(|| target.to_string_lossy().into_owned());
                self.events.saved(&SavedReport {
                    path: &job.path,
                    staged_path: staged.as_deref(),
                    seconds: muxed.secs,
                    frames: muxed.frames,
                    bytes,
                    elapsed_ms: elapsed.as_millis() as u64,
                    video_end_us: job.video_end_us,
                });
                if let (Some(stage), Some(_)) = (&self.stage, &staged) {
                    stage.hand_off(target, &job.path, &self.events);
                }
                // Thumbnail: against the FINAL path, from the
                // request-time frame (nothing here reads the clip file).
                let gallery = Instant::now();
                (self.gallery)(job, muxed.secs, bytes, &self.events);
                timing.gallery_ms = ms(gallery.elapsed());
                let outcome = if staged.is_some() { "staged" } else { "saved" };
                self.events
                    .log(&timing.line(outcome, muxed.secs, muxed.frames, bytes));
                if let Some(line) = crate::gpu_frames::adapter::vram_line("after save") {
                    self.events.log(&line);
                }
            }
            Err(reason) => {
                self.events.save_failed(&reason);
                self.events.log(&timing.line("failed", job.secs as f32, 0, 0));
            }
        }
    }
}

/// `<path>.part`: the in-progress name, renamed to `path` only when complete.
pub(crate) fn part_path(path: &str) -> PathBuf {
    PathBuf::from(format!("{path}.part"))
}

/// Mux into `part`, then rename it to the job's path. Any failure removes the
/// partial file, so a clip path either holds a complete clip or nothing.
pub(crate) fn write_clip<F>(
    part: &Path,
    job: &SaveJob,
    mux: &mut F,
    abort: &AtomicBool,
    timing: &mut Timing,
) -> Result<Muxed, String>
where
    F: FnMut(&Path, &SaveJob, &AtomicBool, &mut Timing) -> Result<Muxed, String> + ?Sized,
{
    write_clip_to(part, Path::new(&job.path), job, mux, abort, timing)
}

/// [`write_clip`] renaming to `target` (the staged file, or the job's path).
pub(crate) fn write_clip_to<F>(
    part: &Path,
    target: &Path,
    job: &SaveJob,
    mux: &mut F,
    abort: &AtomicBool,
    timing: &mut Timing,
) -> Result<Muxed, String>
where
    F: FnMut(&Path, &SaveJob, &AtomicBool, &mut Timing) -> Result<Muxed, String> + ?Sized,
{
    let _ = std::fs::remove_file(part);
    match mux(part, job, abort, timing) {
        Ok(muxed) => {
            let started = Instant::now();
            let renamed = std::fs::rename(part, target);
            timing.rename_ms = ms(started.elapsed());
            match renamed {
                Ok(()) => Ok(muxed),
                Err(e) => {
                    let _ = std::fs::remove_file(part);
                    Err(format!("finalize {}: {e}", target.display()))
                }
            }
        }
        Err(reason) => {
            let _ = std::fs::remove_file(part);
            Err(reason)
        }
    }
}

/// Synchronous save through the production mux + rename path (smoke test).
pub(super) fn write_now(job: &SaveJob) -> Result<Muxed, String> {
    let mut timing = job.timing;
    write_clip(
        &part_path(&job.path),
        job,
        &mut super::mux::produce,
        &AtomicBool::new(false),
        &mut timing,
    )
}

/// Keep the writer below `recall-encoder` so a save never competes with
/// live encoding for a core.
fn lower_thread_priority(events: &Events) {
    use windows::Win32::System::Threading::{
        GetCurrentThread, SetThreadPriority, THREAD_PRIORITY_BELOW_NORMAL,
    };
    // SAFETY: GetCurrentThread returns this thread's pseudo-handle; it needs no
    // closing and is used only by this thread, for this call.
    if let Err(e) = unsafe { SetThreadPriority(GetCurrentThread(), THREAD_PRIORITY_BELOW_NORMAL) } {
        events.log(&format!("saver: could not lower thread priority: {e}"));
    }
    // EcoQoS: Windows keeps the writer on slow cores at low clocks, away from the game.
    crate::thread_qos::eco_background();
}

/// Build a Moments-convention clip path: `<Game>__<YYYY-MM-DD>__<HH-MM-SS>.mp4`.
/// Non-filename characters in the source name become dashes.
pub fn clip_path(out_dir: &str, source: &str) -> String {
    let mut name: String = source
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || c == ' ' || c == '-' || c == '_' {
                c
            } else {
                '-'
            }
        })
        .collect::<String>()
        .trim()
        .replace(' ', "-");
    if name.is_empty() {
        name = "Desktop".to_string();
    }
    let stamp = chrono::Local::now().format("%Y-%m-%d__%H-%M-%S-%3f");
    let suffix = uuid::Uuid::new_v4().simple();
    std::path::Path::new(out_dir)
        .join(format!("{name}__{stamp}__{suffix}.mp4"))
        .to_string_lossy()
        .into_owned()
}

#[cfg(test)]
#[path = "saver_tests.rs"]
mod tests;
