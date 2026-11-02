//! `recall-mover`: moves staged clips (`staging.rs`) from the SSD into the
//! clip folder, one at a time, FIFO, on a below-normal thread in background
//! mode (very low I/O priority, `bg_io.rs`), so the HDD write never competes
//! with anything in the foreground.
//!
//! A move: copy `<staged>` into `<final>.part` in large sequential chunks
//! (space reserved up front, flushed every [`Config::sync_every`] and at the
//! end, stamped with the staged file's modification time), rename to
//! `<final>`, then keep the staged copy as `<staging>\kept\<final name>` and
//! emit `clip_moved` with its `kept_path`: the app adopts it into its SSD video
//! cache, so a fresh clip opens from the SSD at once. If it cannot be kept it
//! is deleted as before. On the same volume a move is a plain rename. A `<final>.part` left by an interrupted
//! move is overwritten by the retry; the final name only ever appears through
//! the rename of a complete, flushed file.
//!
//! Failure keeps the staged copy and parks the move: it is retried on the next
//! save and every [`Config::retry_after`]. Only its first failure emits
//! `clip_move_failed` (one failure cue per clip, not one per retry); later
//! ones are `encoder_log` lines. A staged copy that cannot be deleted after a
//! good move (open elsewhere) is retried after each later move and cleaned at
//! the next start (its destination then already holds the same bytes).
//!
//! Quit: [`MoverControl::shutdown`] aborts an in-flight copy at the next chunk,
//! removes its `.part` and leaves the staged copy for the next start.

use std::collections::VecDeque;
use std::fs::OpenOptions;
use std::io::{Read, Write};
use std::os::windows::fs::OpenOptionsExt;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering::Relaxed};
use std::sync::mpsc::{channel, Receiver, RecvTimeoutError, Sender};
use std::sync::Arc;
use std::time::{Duration, Instant};

use super::events::Events;
use super::saver::ms;
use super::staging::VolumeCheck;
use crate::naming::{part_of, recovered_path};

/// How long quit waits for an aborted copy to stop and clean its `.part`.
pub(crate) const STOP_WAIT: Duration = Duration::from_millis(1500);
const FILE_FLAG_SEQUENTIAL_SCAN: u32 = 0x0800_0000;
const MIB: usize = 1 << 20;

/// One staged clip and where it belongs.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct MoveJob {
    pub staged: PathBuf,
    pub dest: PathBuf,
}

#[derive(Clone, Copy)]
pub(crate) struct Config {
    /// Bytes per read/write.
    pub chunk: usize,
    /// Flush the destination after this many bytes (bounds dirty cache and
    /// keeps the disk writes on this background thread).
    pub sync_every: u64,
    pub retry_after: Duration,
    pub same_volume: VolumeCheck,
    /// Enter background mode (tests that only check queueing may skip it).
    pub background: bool,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            chunk: 8 * MIB,
            sync_every: 32 * MIB as u64,
            retry_after: Duration::from_secs(60),
            same_volume: super::staging::same_volume,
            background: true,
        }
    }
}

enum Msg {
    Move(MoveJob),
    Stop,
}

/// Saver end: hand a staged clip to the mover.
#[derive(Clone)]
pub(crate) struct MoverQueue {
    tx: Sender<Msg>,
}

impl MoverQueue {
    pub fn submit(&self, job: MoveJob) -> bool {
        self.tx.send(Msg::Move(job)).is_ok()
    }
}

/// Main-thread handle used to stop the mover at quit.
pub(crate) struct MoverControl {
    tx: Sender<Msg>,
    abort: Arc<AtomicBool>,
    done: Receiver<()>,
    events: Events,
}

impl MoverControl {
    /// Abort any in-flight copy and wait up to `wait` for the thread to end.
    /// `true` when it stopped (its `.part`, if any, removed).
    pub fn shutdown(self, wait: Duration) -> bool {
        self.abort.store(true, Relaxed);
        let _ = self.tx.send(Msg::Stop);
        let stopped = !matches!(self.done.recv_timeout(wait), Err(RecvTimeoutError::Timeout));
        if !stopped {
            self.events.log("clip mover did not stop in time; a half-copied .part may stay until the retry");
        }
        stopped
    }
}

/// Start `recall-mover` with `recovered` moves queued first. `None` when the
/// thread cannot start (the caller then writes clips in place).
pub(crate) fn spawn(events: Events, cfg: Config, recovered: Vec<MoveJob>) -> Option<(MoverQueue, MoverControl)> {
    let (tx, rx) = channel::<Msg>();
    let (done_tx, done_rx) = channel::<()>();
    let abort = Arc::new(AtomicBool::new(false));
    let mover = Mover {
        events: events.clone(),
        cfg,
        abort: abort.clone(),
        queue: recovered.into_iter().map(Pending::new).collect(),
        parked: Vec::new(),
        retry_at: None,
        leftovers: Vec::new(),
    };
    let spawned = std::thread::Builder::new().name("recall-mover".into()).spawn(move || {
        let _done = done_tx; // dropped when this thread ends: the done signal
        mover.run(rx);
    });
    if let Err(e) = spawned {
        events.log(&format!("clip mover failed to start ({e}); clips are written in place"));
        return None;
    }
    Some((MoverQueue { tx: tx.clone() }, MoverControl { tx, abort, done: done_rx, events }))
}

struct Pending {
    job: MoveJob,
    queued: Instant,
    failures: u32,
}

impl Pending {
    fn new(job: MoveJob) -> Self {
        Pending { job, queued: Instant::now(), failures: 0 }
    }
}

/// Why a move did not complete.
#[derive(Debug, PartialEq)]
pub(crate) enum MoveError {
    /// Retry later; the staged copy is kept.
    Failed(String),
    /// The staged file is gone: nothing left to move.
    Missing,
    /// Quit: the `.part` was removed, the staged copy kept.
    Aborted,
}

/// A completed move.
#[derive(Debug)]
pub(crate) struct Moved {
    /// Where the clip landed (the job's `dest`, unless another file already
    /// held that name: then `<stem>-recovered.mp4`).
    pub dest: PathBuf,
    pub bytes: u64,
    pub how: &'static str,
    pub copy_ms: f64,
    pub flush_ms: f64,
    pub rename_ms: f64,
    /// The staged copy could not be deleted (kept as a leftover).
    pub staged_kept: Option<String>,
    /// The staged copy, kept under `<staging>\kept\` for the app's video cache.
    pub kept: Option<PathBuf>,
}

struct Mover {
    events: Events,
    cfg: Config,
    abort: Arc<AtomicBool>,
    queue: VecDeque<Pending>,
    parked: Vec<Pending>,
    retry_at: Option<Instant>,
    leftovers: Vec<PathBuf>,
}

impl Mover {
    fn run(mut self, rx: Receiver<Msg>) {
        if self.cfg.background {
            self.lower_priority();
        }
        loop {
            if self.abort.load(Relaxed) {
                return;
            }
            if let Some(pending) = self.queue.pop_front() {
                self.attempt(pending);
                continue;
            }
            let wait = self.retry_at.map(|t| t.saturating_duration_since(Instant::now()));
            let msg = match wait {
                Some(wait) => rx.recv_timeout(wait),
                None => rx.recv().map_err(|_| RecvTimeoutError::Disconnected),
            };
            match msg {
                // A new save also retries parked moves, oldest first (FIFO).
                Ok(Msg::Move(job)) => {
                    self.unpark();
                    self.queue.push_back(Pending::new(job));
                }
                Ok(Msg::Stop) | Err(RecvTimeoutError::Disconnected) => return,
                Err(RecvTimeoutError::Timeout) => self.unpark(),
            }
        }
    }

    fn lower_priority(&self) {
        match super::bg_io::enter_background() {
            Ok(()) => self.events.log(&format!(
                "clip mover: background mode on (I/O priority {}, thread priority {})",
                super::bg_io::thread_io_priority().map_or("?".to_string(), |p| p.to_string()),
                super::bg_io::thread_priority()
            )),
            Err(e) => self.events.log(&format!("clip mover: could not lower priority: {e}")),
        }
    }

    fn unpark(&mut self) {
        self.retry_at = None;
        let parked = std::mem::take(&mut self.parked);
        for (i, pending) in parked.into_iter().enumerate() {
            self.queue.insert(i, pending);
        }
        self.leftovers.retain(|p| std::fs::remove_file(p).is_err() && p.exists());
    }

    fn attempt(&mut self, mut pending: Pending) {
        let started = Instant::now();
        let job = &pending.job;
        let result = catch_unwind(AssertUnwindSafe(|| move_clip(job, &self.cfg, &self.abort)))
            .unwrap_or_else(|_| Err(MoveError::Failed("internal error while moving the clip".into())));
        let (dest, staged) = (job.dest.to_string_lossy().into_owned(), job.staged.to_string_lossy().into_owned());
        match result {
            Ok(moved) => {
                let elapsed = started.elapsed();
                let kept = moved.kept.as_ref().map(|p| p.to_string_lossy().into_owned());
                self.events.clip_moved(&moved.dest.to_string_lossy(), elapsed.as_millis() as u64, kept.as_deref());
                let queue_wait = started.saturating_duration_since(pending.queued);
                self.events.log(&timing_line(&moved, ms(elapsed), ms(queue_wait)));
                if let Some(e) = &moved.staged_kept {
                    self.events.log(&format!("clip mover: staged copy {staged} not deleted yet ({e})"));
                    self.leftovers.push(job.staged.clone());
                }
                self.leftovers.retain(|p| std::fs::remove_file(p).is_err() && p.exists());
            }
            Err(MoveError::Failed(reason)) => {
                pending.failures += 1;
                if pending.failures == 1 {
                    self.events.clip_move_failed(&dest, &staged, &reason);
                } else {
                    let n = pending.failures;
                    self.events.log(&format!("clip move retry {n} failed for {dest}: {reason}"));
                }
                self.retry_at.get_or_insert(Instant::now() + self.cfg.retry_after);
                self.parked.push(pending);
            }
            Err(MoveError::Missing) => self.events.clip_move_failed(&dest, &staged, "staged file is missing; nothing to move"),
            Err(MoveError::Aborted) => {
                self.events.log(&format!("clip move to {dest} stopped at quit; {staged} stays staged for the next start"));
            }
        }
    }
}

fn timing_line(m: &Moved, total_ms: f64, queue_wait_ms: f64) -> String {
    let mb_s = if m.copy_ms > 0.0 { m.bytes as f64 / 1e6 / (m.copy_ms / 1000.0) } else { 0.0 };
    format!(
        "move timing ({}): bytes={} queue_wait={queue_wait_ms:.1}ms copy={:.1}ms ({mb_s:.1} MB/s) \
         flush={:.1}ms rename={:.1}ms total={total_ms:.1}ms -> {}",
        m.how,
        m.bytes,
        m.copy_ms,
        m.flush_ms,
        m.rename_ms,
        m.dest.display()
    )
}

fn failed(what: &str, path: &Path, e: std::io::Error) -> MoveError {
    MoveError::Failed(format!("{what} {}: {e}", path.display()))
}

/// Move one staged clip (see the module doc). Never leaves a partial file
/// under the destination name; on any error the `.part` is removed.
pub(crate) fn move_clip(job: &MoveJob, cfg: &Config, abort: &AtomicBool) -> Result<Moved, MoveError> {
    let len = match std::fs::metadata(&job.staged) {
        Ok(m) => m.len(),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Err(MoveError::Missing),
        Err(e) => return Err(failed("read", &job.staged, e)),
    };
    let mut moved = Moved {
        dest: job.dest.clone(),
        bytes: len,
        how: "copy",
        copy_ms: 0.0,
        flush_ms: 0.0,
        rename_ms: 0.0,
        staged_kept: None,
        kept: None,
    };
    match std::fs::metadata(&job.dest).map(|m| m.len()) {
        // The rename landed before a crash or quit kept the staged copy.
        Ok(existing) if existing == len => moved.how = "already in place",
        // Never overwrite a different finished file: land beside it.
        Ok(_) => moved.dest = recovered_path(&part_of(&job.dest), Path::exists),
        Err(_) => {}
    }
    let out_dir = moved.dest.parent().unwrap_or(Path::new("."));
    if moved.how != "already in place" && (cfg.same_volume)(&job.staged, out_dir) {
        moved.how = "rename";
        let started = Instant::now();
        std::fs::rename(&job.staged, &moved.dest).map_err(|e| failed("move to", &moved.dest, e))?;
        moved.rename_ms = ms(started.elapsed());
        return Ok(moved);
    }
    if moved.how == "copy" {
        copy_into_place(&job.staged, &mut moved, cfg, abort)?;
        moved.kept = keep_staged(&job.staged, &moved.dest);
        if moved.kept.is_some() {
            return Ok(moved);
        }
    }
    if let Err(e) = std::fs::remove_file(&job.staged) {
        moved.staged_kept = Some(e.to_string());
    }
    Ok(moved)
}

/// `<staging>\kept`: staged copies of moved clips, waiting for the app.
/// `recover` scans only the staging folder's own files, so these never move again.
pub(crate) const KEPT_DIR: &str = "kept";

/// Renames the staged copy to `<staging>\kept\<dest file name>`; `None` (the
/// caller deletes it) when that folder or the rename fails.
fn keep_staged(staged: &Path, dest: &Path) -> Option<PathBuf> {
    let dir = staged.parent()?.join(KEPT_DIR);
    let kept = dir.join(dest.file_name()?);
    std::fs::create_dir_all(&dir).ok()?;
    std::fs::rename(staged, &kept).ok()?;
    Some(kept)
}

/// Copy `staged` to `<dest>.part`, flush, rename to `dest`.
fn copy_into_place(staged: &Path, moved: &mut Moved, cfg: &Config, abort: &AtomicBool) -> Result<(), MoveError> {
    let part = part_of(&moved.dest);
    let result = copy_to_part(staged, &part, moved, cfg, abort).and_then(|()| {
        let started = Instant::now();
        std::fs::rename(&part, &moved.dest).map_err(|e| failed("finalize", &moved.dest, e))?;
        moved.rename_ms = ms(started.elapsed());
        Ok(())
    });
    if result.is_err() {
        let _ = std::fs::remove_file(&part);
    }
    result
}

fn copy_to_part(staged: &Path, part: &Path, moved: &mut Moved, cfg: &Config, abort: &AtomicBool) -> Result<(), MoveError> {
    let started = Instant::now();
    let mut input = OpenOptions::new()
        .read(true)
        .custom_flags(FILE_FLAG_SEQUENTIAL_SCAN)
        .open(staged)
        .map_err(|e| failed("open", staged, e))?;
    // create + truncate: a `.part` from an interrupted move is overwritten.
    let mut output = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .custom_flags(FILE_FLAG_SEQUENTIAL_SCAN)
        .open(part)
        .map_err(|e| failed("create", part, e))?;
    // Reserve the space first: a full disk fails here, not 400 MB in.
    output.set_len(moved.bytes).map_err(|e| failed("reserve space for", part, e))?;
    let mut buf = vec![0u8; cfg.chunk.max(4096)];
    let (mut copied, mut unsynced) = (0u64, 0u64);
    loop {
        if abort.load(Relaxed) {
            return Err(MoveError::Aborted);
        }
        let n = input.read(&mut buf).map_err(|e| failed("read", staged, e))?;
        if n == 0 {
            break;
        }
        output.write_all(&buf[..n]).map_err(|e| failed("write", part, e))?;
        copied += n as u64;
        unsynced += n as u64;
        if unsynced >= cfg.sync_every {
            output.sync_data().map_err(|e| failed("flush", part, e))?;
            unsynced = 0;
        }
    }
    if copied != moved.bytes {
        return Err(MoveError::Failed(format!("staged file changed size while moving ({copied} of {} bytes)", moved.bytes)));
    }
    moved.copy_ms = ms(started.elapsed());
    let flush = Instant::now();
    output.sync_all().map_err(|e| failed("flush", part, e))?;
    moved.flush_ms = ms(flush.elapsed());
    // The clip keeps its save time, so the app's cache identity (name, size,
    // mtime) of `dest` matches the kept copy from the moment it appears.
    if let Ok(modified) = input.metadata().and_then(|m| m.modified()) {
        output.set_modified(modified).map_err(|e| failed("stamp", part, e))?;
    }
    Ok(())
}

#[cfg(test)]
#[path = "mover_tests.rs"]
mod tests;
