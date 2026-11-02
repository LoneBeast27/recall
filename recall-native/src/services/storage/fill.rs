//! Progressive clip fill: while a clip on a hard drive plays, one reader copies it to an SSD
//! file, 4 MiB at a time, and everything else (the player, the sound decoder, the timeline)
//! reads that file. The drive never sees a second reader, so nothing seeks against the copy.
//!
//! The index of a recorder clip sits at the end of the file, so the first reads are the index
//! and the start of the picture. After that the reader runs in file order; a read that lands
//! on a part still missing moves it there (a seek ahead of the copy), and when it reaches the
//! end it wraps round to whatever was skipped. Each chunk is one lane task, so hover frames and
//! thumbnails get the disk between chunks unless the player is waiting.
//!
//! The file is sparse and unbuffered on both sides: a hole never reaches a reader, and the
//! file cache never holds zeros for a part written later.
//!
//! While the recorder needs the drive, or nobody is watching, the copy never stops: it goes on
//! at [`PACED_RATE`] on the lane's background I/O, so a clip opened mid-game still loads fully
//! on its own while the recorder's writes keep most of the disk. A reader stopped on a missing
//! part is always served at full speed.
//!
//! A clip too large for the video tier is not copied ([`ClipFill::direct`]): the same readers
//! read it where it lies, so the player and the decoder need no second path.
use std::fs::OpenOptions;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::time::{Duration, Instant, SystemTime};

use super::lane::{self, Priority};

#[path = "fill_reader.rs"]
mod reader;
#[path = "fill_player.rs"]
mod player;
pub use reader::AlignedBuffer;
use reader::{Reader, open_part, open_unbuffered, set_sparse};

/// One read of the drive. Also the unit of the "have it" map.
pub const CHUNK: u64 = 4 << 20;
/// Background copying while paced ([`ClipFill::pace`]): 40 MB/s, a 400 MB clip in ~10 s,
/// leaving a hard drive most of its ~150 MB/s for the recorder.
pub const PACED_RATE: u64 = 40_000_000;
pub(super) const ALIGN: u64 = 4096;
/// Bytes kept ready past the player's last read before the copy yields to other disk work.
const RUNWAY: u64 = 64 << 20;
/// After a seek, how long reads of what will never arrive answer at once.
const RELEASE: Duration = Duration::from_millis(500);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    Filling,
    Done,
    Cancelled,
    /// The drive stopped answering, or the clip changed: what arrived stays playable.
    Failed,
}

/// Part files of the fills alive in this process: nothing may clean these up.
static IN_USE: Mutex<Vec<PathBuf>> = Mutex::new(Vec::new());

/// A fill in this process is writing `path`.
pub(super) fn in_use(path: &Path) -> bool {
    IN_USE.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).iter().any(|part| part == path)
}

struct State {
    have: Vec<bool>,
    missing: u64,
    /// Next chunk in file order.
    cursor: u64,
    /// Chunks a reader is stopped on, newest last.
    wanted: Vec<u64>,
    /// Last chunk the player read: how far ahead of it the copy has got decides its priority.
    played: u64,
    outcome: Outcome,
    error: Option<String>,
    /// Background copying runs at [`PACED_RATE`] (the editor is not showing the clip, or the
    /// recorder is busy). A reader stopped on a missing part is served at full speed.
    paced: bool,
    /// The last seek of the player, while reads wait on what will not arrive.
    sought: Option<std::time::Instant>,
}

/// What the reader does next.
enum Next {
    /// A chunk to copy, and whether to pace it.
    Chunk(u64, bool),
    /// The fill stopped (done, failed or cancelled).
    Park,
    Finished,
}

struct Shared {
    source: PathBuf,
    part: PathBuf,
    target: PathBuf,
    bytes: u64,
    modified: SystemTime,
    state: Mutex<State>,
    changed: Condvar,
    /// Each change of the map, so the UI repaints only when there is something new.
    revision: AtomicU64,
    /// Test hooks: bytes a second the reader may take (0 = as fast as the drive goes), and
    /// bytes after which the drive stops answering (0 = never).
    throttle: u64,
    fail_after: u64,
    /// The clip read where it lies: `part` is the clip itself and must never be removed.
    direct: bool,
    playing: AtomicBool,
    reader: Mutex<Option<Arc<Mutex<Reader>>>>,
}

impl Drop for Shared {
    fn drop(&mut self) {
        if self.direct {
            return;
        }
        IN_USE.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).retain(|part| *part != self.part);
        if !self.state.get_mut().is_ok_and(|state| state.outcome == Outcome::Done) {
            let _ = std::fs::remove_file(&self.part);
        }
    }
}

/// One clip being copied. Clones share the fill.
#[derive(Clone)]
pub struct ClipFill(Arc<Shared>);

fn lock(shared: &Shared) -> MutexGuard<'_, State> {
    shared.state.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn chunk_count(bytes: u64) -> u64 {
    bytes.div_ceil(CHUNK).max(1)
}

impl ClipFill {
    /// Prepares `part` (sparse, full length) beside `target`; call [`ClipFill::start`] to begin.
    pub fn create(source: &Path, part: &Path, target: &Path, bytes: u64, modified: SystemTime, throttle: u64, fail_after: u64) -> Result<Self, String> {
        let file = OpenOptions::new().write(true).create(true).truncate(true).open(part).map_err(|error| format!("create fill: {error}"))?;
        set_sparse(&file)?;
        file.set_len(bytes).map_err(|error| format!("size fill: {error}"))?;
        IN_USE.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).push(part.to_path_buf());
        let state = State { have: vec![false; chunk_count(bytes) as usize], missing: chunk_count(bytes), cursor: 0, wanted: Vec::new(), played: 0, outcome: Outcome::Filling, error: None, paced: false, sought: None };
        Ok(Self::shared(source, part, target, bytes, modified, state, throttle, fail_after, false))
    }

    /// The clip where it lies: nothing is copied, everything counts as there. `fail_after`: the
    /// test hook: after the player's first seek, a read past it finds the drive gone (the index,
    /// at the end of a recorder clip, has to open and prime first).
    pub fn direct(source: &Path, bytes: u64, modified: SystemTime, fail_after: u64) -> Self {
        let state = State { have: vec![true; chunk_count(bytes) as usize], missing: 0, cursor: 0, wanted: Vec::new(), played: 0, outcome: Outcome::Done, error: None, paced: false, sought: None };
        Self::shared(source, source, source, bytes, modified, state, 0, fail_after, true)
    }

    #[allow(clippy::too_many_arguments)]
    fn shared(source: &Path, part: &Path, target: &Path, bytes: u64, modified: SystemTime, state: State, throttle: u64, fail_after: u64, direct: bool) -> Self {
        Self(Arc::new(Shared {
            source: source.to_path_buf(),
            part: part.to_path_buf(),
            target: target.to_path_buf(),
            bytes,
            modified,
            state: Mutex::new(state),
            changed: Condvar::new(),
            revision: AtomicU64::new(0),
            throttle,
            fail_after,
            direct,
            playing: AtomicBool::new(false),
            reader: Mutex::new(None),
        }))
    }

    /// Asks for `chunks` first (the index and the head of the clip) and starts reading.
    pub fn start(&self, first: &[u64]) -> Result<(), String> {
        let input = open_unbuffered(&self.0.source).map_err(|error| format!("open clip: {error}"))?;
        let output = open_part(&self.0.part).map_err(|error| format!("open fill: {error}"))?;
        {
            let mut state = lock(&self.0);
            state.wanted = first.iter().rev().copied().collect();
        }
        let reader = Arc::new(Mutex::new(Reader::new(input, output)));
        *self.0.reader.lock().unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(Arc::clone(&reader));
        schedule(self.clone(), reader, Priority::Interactive);
        Ok(())
    }

    /// The clip is read where it lies, not copied.
    pub fn is_direct(&self) -> bool {
        self.0.direct
    }

    /// Caps background copying at [`PACED_RATE`], or lets it run at the drive's speed. The
    /// copy never stops for this: it always finishes on its own.
    pub fn pace(&self, paced: bool) {
        lock(&self.0).paced = paced;
    }

    pub fn bytes(&self) -> u64 {
        self.0.bytes
    }

    /// The clip being read.
    pub fn source(&self) -> &Path {
        &self.0.source
    }

    /// Where the finished copy lands.
    pub fn target(&self) -> &Path {
        &self.0.target
    }

    pub fn revision(&self) -> u64 {
        self.0.revision.load(Ordering::Relaxed)
    }

    pub fn outcome(&self) -> Outcome {
        lock(&self.0).outcome
    }

    pub fn error(&self) -> Option<String> {
        lock(&self.0).error.clone()
    }

    /// Share of the clip on the SSD (0..=1).
    pub fn progress(&self) -> f64 {
        let state = lock(&self.0);
        1.0 - state.missing as f64 / state.have.len() as f64
    }

    /// `[start, end)` is on the SSD.
    pub fn has(&self, start: u64, end: u64) -> bool {
        let state = lock(&self.0);
        spanning(start, end, self.0.bytes).all(|chunk| state.have[chunk as usize])
    }

    /// The bytes not on the SSD yet, as `[start, end)` runs in file order.
    pub fn holes(&self) -> Vec<(u64, u64)> {
        let state = lock(&self.0);
        let mut holes: Vec<(u64, u64)> = Vec::new();
        for (index, have) in state.have.iter().enumerate() {
            if *have {
                continue;
            }
            let start = index as u64 * CHUNK;
            let end = (start + CHUNK).min(self.0.bytes);
            match holes.last_mut() {
                Some(last) if last.1 == start => last.1 = end,
                _ => holes.push((start, end)),
            }
        }
        holes
    }

    /// Stops the reader. Readers waiting on this fill return false.
    pub fn cancel(&self) {
        let mut state = lock(&self.0);
        if matches!(state.outcome, Outcome::Filling | Outcome::Failed) {
            state.outcome = Outcome::Cancelled;
        }
        drop(state);
        self.0.changed.notify_all();
    }

    /// Waits until `[start, end)` is there. `want` moves the reader to it (the player does;
    /// the sound decoder only follows). False when the fill stopped first.
    pub fn wait_for(&self, start: u64, end: u64, want: bool) -> bool {
        let chunks: Vec<u64> = spanning(start, end, self.0.bytes).collect();
        let mut state = lock(&self.0);
        if want {
            state.played = chunks.last().copied().unwrap_or(0);
        }
        loop {
            let missing: Vec<u64> = chunks.iter().copied().filter(|chunk| !state.have[*chunk as usize]).collect();
            if missing.is_empty() {
                return true;
            }
            if state.outcome != Outcome::Filling {
                return false;
            }
            if want {
                state.wanted.retain(|chunk| !missing.contains(chunk));
                state.wanted.extend(missing.iter().rev());
            }
            state = self.0.changed.wait_timeout(state, Duration::from_millis(200)).unwrap_or_else(|poisoned| poisoned.into_inner()).0;
        }
    }

    /// Waits for the whole clip. True when the copy is complete.
    pub fn wait_done(&self) -> bool {
        let mut state = lock(&self.0);
        while state.outcome == Outcome::Filling {
            state = self.0.changed.wait_timeout(state, Duration::from_millis(500)).unwrap_or_else(|poisoned| poisoned.into_inner()).0;
        }
        state.outcome == Outcome::Done
    }

    /// Takes the next chunk: a stopped reader's first (urgent), else the next in file order,
    /// wrapping to whatever was skipped (paced when [`ClipFill::pace`] says so).
    fn next_chunk(&self) -> Next {
        let mut state = lock(&self.0);
        if state.outcome != Outcome::Filling {
            return Next::Park;
        }
        while let Some(chunk) = state.wanted.pop() {
            if !state.have[chunk as usize] {
                // The copy carries on from here, so a seek ahead keeps playing.
                state.cursor = chunk + 1;
                return Next::Chunk(chunk, false);
            }
        }
        let paced = state.paced;
        let total = state.have.len() as u64;
        match (0..total).map(|step| (state.cursor + step) % total).find(|chunk| !state.have[*chunk as usize]) {
            Some(chunk) => {
                state.cursor = chunk + 1;
                Next::Chunk(chunk, paced)
            }
            None => Next::Finished,
        }
    }

    /// How urgently the next chunk is needed: a stopped reader or a short runway beat
    /// thumbnails and hover frames; a comfortable lead gives them the disk.
    fn priority(&self) -> Priority {
        let state = lock(&self.0);
        if !state.wanted.is_empty() {
            return Priority::Interactive;
        }
        let lead = (state.played..state.have.len() as u64).take_while(|chunk| state.have[*chunk as usize]).count() as u64 * CHUNK;
        let finished = state.played + lead / CHUNK >= state.have.len() as u64;
        if lead < RUNWAY && !finished { Priority::Interactive } else { Priority::Background }
    }

    fn mark(&self, chunk: u64) {
        let mut state = lock(&self.0);
        if !state.have[chunk as usize] {
            state.have[chunk as usize] = true;
            state.missing -= 1;
        }
        drop(state);
        self.0.revision.fetch_add(1, Ordering::Relaxed);
        self.0.changed.notify_all();
    }

    fn fail(&self, error: String) {
        let mut state = lock(&self.0);
        state.error = Some(error);
        if state.outcome == Outcome::Filling {
            state.outcome = Outcome::Failed;
        }
        drop(state);
        self.0.revision.fetch_add(1, Ordering::Relaxed);
        self.0.changed.notify_all();
    }

    /// Stamps the copy and publishes it. The open readers keep working (shared for delete).
    fn finish(&self, reader: &mut Reader) -> Result<(), String> {
        reader.output.set_len(self.0.bytes).map_err(|error| format!("trim fill: {error}"))?;
        reader.output.set_modified(self.0.modified).map_err(|error| format!("stamp fill: {error}"))?;
        let after = self.0.source.metadata().map_err(|error| format!("stat clip: {error}"))?;
        if after.len() != self.0.bytes || after.modified().ok() != Some(self.0.modified) {
            return Err("Clip changed while it was being copied.".into());
        }
        if self.0.target != self.0.part {
            super::ffmpeg::publish(&self.0.part, &self.0.target)?;
        }
        Ok(())
    }

    fn step(&self, reader: &mut Reader) -> bool {
        let started = Instant::now();
        let (chunk, paced) = match self.next_chunk() {
            Next::Chunk(chunk, paced) => (chunk, paced),
            Next::Park => return false,
            Next::Finished => {
                match self.finish(reader) {
                    Ok(()) => {
                        let mut state = lock(&self.0);
                        if state.outcome == Outcome::Filling {
                            state.outcome = Outcome::Done;
                        }
                    }
                    Err(error) => self.fail(error),
                }
                self.0.revision.fetch_add(1, Ordering::Relaxed);
                self.0.changed.notify_all();
                return false;
            }
        };
        if let Err(error) = reader.copy(chunk, self.0.bytes, self.0.throttle, self.0.fail_after) {
            self.fail(error);
            return false;
        }
        self.mark(chunk);
        if paced {
            let length = (self.0.bytes - chunk * CHUNK).min(CHUNK);
            let due = Duration::from_secs_f64(length as f64 / PACED_RATE as f64);
            if let Some(rest) = due.checked_sub(started.elapsed()) {
                std::thread::sleep(rest);
            }
        }
        true
    }
}

/// The chunks of `[start, end)`, clamped to the clip.
fn spanning(start: u64, end: u64, bytes: u64) -> impl Iterator<Item = u64> {
    let last = chunk_count(bytes) - 1;
    let first = (start / CHUNK).min(last);
    let stop = (end.max(start + 1).saturating_sub(1) / CHUNK).min(last);
    first..=stop
}

/// One step, then the next: on the clip's disk lane, or a thread of its own on an SSD (the
/// slowed-drive test), where a lane would run the steps inline.
fn schedule(fill: ClipFill, reader: Arc<Mutex<Reader>>, priority: Priority) {
    let Some(disk) = lane::hdd_lane(&fill.0.source) else {
        let spawned = std::thread::Builder::new().name("recall-clip-fill".into()).spawn(move || {
            let mut reader = reader.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
            while fill.step(&mut reader) {}
        });
        if let Err(error) = spawned {
            eprintln!("start clip fill: {error}");
        }
        return;
    };
    disk.submit(priority, Box::new(move || {
        let more = fill.step(&mut reader.lock().unwrap_or_else(|poisoned| poisoned.into_inner()));
        if more {
            let next = fill.priority();
            schedule(fill, reader, next);
        }
    }));
}

#[cfg(test)]
#[path = "tests_fill.rs"]
mod tests;
