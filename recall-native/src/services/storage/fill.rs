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
use std::alloc::{Layout, alloc_zeroed, dealloc, handle_alloc_error};
use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::os::windows::fs::{FileExt, OpenOptionsExt};
use std::os::windows::io::AsRawHandle;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::time::{Duration, Instant, SystemTime};

use windows::Win32::Foundation::HANDLE;
use windows::Win32::Storage::FileSystem::{FILE_FLAG_NO_BUFFERING, FILE_FLAG_WRITE_THROUGH};
use windows::Win32::System::IO::DeviceIoControl;

use super::lane::{self, Priority};

/// One read of the drive. Also the unit of the "have it" map.
pub const CHUNK: u64 = 4 << 20;
const ALIGN: u64 = 4096;
const FSCTL_SET_SPARSE: u32 = 0x0009_00C4;
/// Bytes kept ready past the player's last read before the copy yields to other disk work.
const RUNWAY: u64 = 64 << 20;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    Filling,
    Done,
    Cancelled,
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
    /// Test hook: bytes a second the reader may take (0 = as fast as the drive goes).
    throttle: u64,
    /// Over the video tier's cap: the copy leaves with the clip.
    session: bool,
}

impl Drop for Shared {
    fn drop(&mut self) {
        let done = self.state.get_mut().is_ok_and(|state| state.outcome == Outcome::Done);
        if !done {
            let _ = std::fs::remove_file(&self.part);
        } else if self.session {
            let _ = std::fs::remove_file(&self.target);
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
    pub fn create(source: &Path, part: &Path, target: &Path, bytes: u64, modified: SystemTime, throttle: u64, session: bool) -> Result<Self, String> {
        let file = OpenOptions::new().write(true).create(true).truncate(true).open(part).map_err(|error| format!("create fill: {error}"))?;
        set_sparse(&file)?;
        file.set_len(bytes).map_err(|error| format!("size fill: {error}"))?;
        let state = State { have: vec![false; chunk_count(bytes) as usize], missing: chunk_count(bytes), cursor: 0, wanted: Vec::new(), played: 0, outcome: Outcome::Filling, error: None };
        Ok(Self(Arc::new(Shared {
            source: source.to_path_buf(),
            part: part.to_path_buf(),
            target: target.to_path_buf(),
            bytes,
            modified,
            state: Mutex::new(state),
            changed: Condvar::new(),
            revision: AtomicU64::new(0),
            throttle,
            session,
        })))
    }

    /// Asks for `chunks` first (the index and the head of the clip) and starts reading.
    pub fn start(&self, first: &[u64]) -> Result<(), String> {
        let input = open_unbuffered(&self.0.source).map_err(|error| format!("open clip: {error}"))?;
        let output = open_part(&self.0.part).map_err(|error| format!("open fill: {error}"))?;
        {
            let mut state = lock(&self.0);
            state.wanted = first.iter().rev().copied().collect();
        }
        let reader = Arc::new(Mutex::new(Reader { input, output, buffer: AlignedBuffer::new(CHUNK as usize), last: None, started: Instant::now(), read: 0 }));
        schedule(self.clone(), reader, Priority::Interactive);
        Ok(())
    }

    pub fn bytes(&self) -> u64 {
        self.0.bytes
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
        if state.outcome == Outcome::Filling {
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

    /// Reads `[offset, ...)` from the SSD file. The range must be there (`wait_for`).
    pub fn read_at(&self, file: &File, offset: u64, out: &mut [u8], scratch: &mut AlignedBuffer) -> std::io::Result<usize> {
        let length = (out.len() as u64).min(self.0.bytes.saturating_sub(offset));
        let aligned = offset / ALIGN * ALIGN;
        let skip = (offset - aligned) as usize;
        let span = ((skip as u64 + length).div_ceil(ALIGN) * ALIGN).min(scratch.bytes().len() as u64) as usize;
        let mut got = 0;
        while got < span {
            let read = file.seek_read(&mut scratch.bytes()[got..span], aligned + got as u64)?;
            if read == 0 {
                break;
            }
            got += read;
        }
        let usable = got.saturating_sub(skip).min(length as usize);
        out[..usable].copy_from_slice(&scratch.bytes()[skip..skip + usable]);
        Ok(usable)
    }

    /// A handle for [`ClipFill::read_at`].
    pub fn open_reader(&self) -> std::io::Result<File> {
        OpenOptions::new().read(true).custom_flags(FILE_FLAG_NO_BUFFERING.0).share_mode(7).open(&self.0.part)
    }

    /// Takes the next chunk: a stopped reader's first, else the next in file order, wrapping
    /// to whatever was skipped.
    fn next_chunk(&self) -> Option<(u64, bool)> {
        let mut state = lock(&self.0);
        if state.outcome != Outcome::Filling {
            return None;
        }
        while let Some(chunk) = state.wanted.pop() {
            if !state.have[chunk as usize] {
                // The copy carries on from here, so a seek ahead keeps playing.
                state.cursor = chunk + 1;
                return Some((chunk, true));
            }
        }
        let total = state.have.len() as u64;
        (0..total).map(|step| (state.cursor + step) % total).find(|chunk| !state.have[*chunk as usize]).map(|chunk| {
            state.cursor = chunk + 1;
            (chunk, false)
        })
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
        state.outcome = Outcome::Cancelled;
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
        let Some((chunk, _)) = self.next_chunk() else {
            let complete = lock(&self.0).missing == 0;
            if complete {
                let result = self.finish(reader);
                let mut state = lock(&self.0);
                match result {
                    Ok(()) if state.outcome == Outcome::Filling => state.outcome = Outcome::Done,
                    Ok(()) => {}
                    Err(error) => {
                        state.error = Some(error);
                        state.outcome = Outcome::Cancelled;
                    }
                }
                drop(state);
                self.0.changed.notify_all();
            }
            return false;
        };
        if let Err(error) = reader.copy(chunk, self.0.bytes, self.0.throttle) {
            self.fail(error);
            return false;
        }
        self.mark(chunk);
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

struct Reader {
    input: File,
    output: File,
    buffer: AlignedBuffer,
    /// Chunk read last: the next one needs no seek when it follows.
    last: Option<u64>,
    started: Instant,
    read: u64,
}

impl Reader {
    fn copy(&mut self, chunk: u64, bytes: u64, throttle: u64) -> Result<(), String> {
        let offset = chunk * CHUNK;
        let want = (bytes - offset).min(CHUNK);
        if self.last.is_none_or(|last| last + 1 != chunk) {
            self.input.seek(SeekFrom::Start(offset)).map_err(|error| format!("seek clip: {error}"))?;
        }
        self.last = Some(chunk);
        let padded = want.div_ceil(ALIGN) * ALIGN;
        let mut got = 0usize;
        while (got as u64) < want {
            let read = self.input.read(&mut self.buffer.bytes()[got..padded as usize]).map_err(|error| format!("read clip: {error}"))?;
            if read == 0 {
                return Err("clip ended early".into());
            }
            got += read;
            if (got as u64) < want && got as u64 % ALIGN != 0 {
                return Err("short unaligned read".into());
            }
        }
        self.buffer.bytes()[got..padded as usize].fill(0);
        self.output.seek(SeekFrom::Start(offset)).map_err(|error| format!("seek fill: {error}"))?;
        self.output.write_all(&self.buffer.bytes()[..padded as usize]).map_err(|error| format!("write fill: {error}"))?;
        lane::count_read(want);
        self.read += want;
        if throttle > 0 {
            let due = Duration::from_secs_f64(self.read as f64 / throttle as f64);
            let elapsed = self.started.elapsed();
            if due > elapsed {
                std::thread::sleep(due - elapsed);
            }
        }
        Ok(())
    }
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

fn open_unbuffered(path: &Path) -> std::io::Result<File> {
    OpenOptions::new().read(true).custom_flags(FILE_FLAG_NO_BUFFERING.0).open(path)
}

fn open_part(path: &Path) -> std::io::Result<File> {
    OpenOptions::new().write(true).custom_flags(FILE_FLAG_NO_BUFFERING.0 | FILE_FLAG_WRITE_THROUGH.0).open(path)
}

fn set_sparse(file: &File) -> Result<(), String> {
    let handle = HANDLE(file.as_raw_handle());
    let mut returned = 0u32;
    unsafe { DeviceIoControl(handle, FSCTL_SET_SPARSE, None, 0, None, 0, Some(&mut returned), None) }.map_err(|error| format!("make fill sparse: {error}"))
}

/// 4 KiB-aligned heap buffer for unbuffered I/O.
pub struct AlignedBuffer {
    ptr: *mut u8,
    layout: Layout,
}

// The buffer is plain memory owned by one user at a time.
unsafe impl Send for AlignedBuffer {}

impl AlignedBuffer {
    pub fn new(len: usize) -> Self {
        let layout = Layout::from_size_align(len, ALIGN as usize).expect("aligned layout");
        let ptr = unsafe { alloc_zeroed(layout) };
        if ptr.is_null() {
            handle_alloc_error(layout);
        }
        Self { ptr, layout }
    }

    pub fn bytes(&mut self) -> &mut [u8] {
        unsafe { std::slice::from_raw_parts_mut(self.ptr, self.layout.size()) }
    }
}

impl Drop for AlignedBuffer {
    fn drop(&mut self) {
        unsafe { dealloc(self.ptr, self.layout) };
    }
}

#[cfg(test)]
#[path = "tests_fill.rs"]
mod tests;
