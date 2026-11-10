//! RAM budget for the replay ring, and the disk file its oldest payloads spill to.
//!
//! The ring keeps every packet header in RAM, so saves, eviction and timing work as
//! before. Only payloads move: once the RAM the ring holds passes its budget, the
//! oldest RAM payloads are queued to the `recall-spill` thread, which appends them to a
//! circular file and switches each one to a (file, offset) reference. A save reads
//! those back one packet at a time while muxing.
//!
//! Budget (`Budget`): one eighth of installed RAM, minus the audio rings, at least
//! 128 MiB; `RECALL_RAM_BUFFER_MB` overrides it. While free RAM is below its floor
//! (1.5 GiB or 10% of RAM, whichever is larger) the budget shrinks so the game is
//! not paged out. On most PCs a default buffer (120 s at 25 Mbps, ~375 MB) never
//! leaves RAM and no spill file is ever created.
//!
//! Spill drive (`drives.rs`): a different physical disk from the game first (SSD
//! before HDD), the game's own disk only when nothing else fits, never a drive with
//! less than `KEEP_FREE` left. The file is opened delete-on-close, so it disappears
//! when the engine exits or crashes; a power cut or BSOD leaves it, and the next start
//! removes it (`sweep.rs`).
//!
//! When neither RAM nor disk can hold the asked-for length, the ring drops its
//! oldest seconds instead (the pre-spill behavior with a byte cap).

mod drives;
mod sweep;
pub(crate) use sweep::is_recall;

use std::fs::File;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering::{Acquire, Relaxed, Release}};
use std::sync::mpsc::{channel, Sender};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

const MIB: u64 = 1024 * 1024;
const GIB: u64 = 1024 * MIB;
/// Never leave a drive with less than this free because of the spill file.
pub(crate) const KEEP_FREE: u64 = 4 * GIB;
/// Smallest spill file worth creating.
const MIN_FILE: u64 = 64 * MIB;
/// Free-RAM checks are this far apart (GlobalMemoryStatusEx is cheap, but not per packet).
const RECHECK: Duration = Duration::from_secs(2);

/// One encoded packet's bytes: in RAM, or in the spill file.
#[derive(Clone)]
pub struct Payload(Arc<Chunk>);

struct Chunk {
    len: usize,
    state: Mutex<State>,
    /// The RAM gauge this chunk counts against while its bytes are in RAM.
    gauge: OnceLock<Arc<AtomicUsize>>,
}

enum State {
    Ram(Arc<[u8]>),
    Disk { file: Arc<SpillFile>, at: u64 },
}

impl Payload {
    pub fn len(&self) -> usize {
        self.0.len
    }

    /// The bytes, read back from the spill file when they were moved there.
    pub fn read(&self) -> Result<Arc<[u8]>, String> {
        let state = self.0.state.lock().unwrap_or_else(|e| e.into_inner());
        match &*state {
            State::Ram(bytes) => Ok(Arc::clone(bytes)),
            State::Disk { file, at } => file.read(*at, self.0.len).map(Arc::from),
        }
    }

    /// True when the bytes are in RAM.
    pub fn in_ram(&self) -> bool {
        matches!(&*self.0.state.lock().unwrap_or_else(|e| e.into_inner()), State::Ram(_))
    }

    /// True when the spill file has since overwritten these bytes.
    pub fn lost(&self) -> bool {
        match &*self.0.state.lock().unwrap_or_else(|e| e.into_inner()) {
            State::Ram(_) => false,
            State::Disk { file, at } => !file.holds(*at),
        }
    }

    /// Same allocation (a save window shares the ring's payloads).
    #[cfg(test)]
    pub fn same(&self, other: &Payload) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }

    /// Handles to this payload (the ring's own included).
    pub fn holders(&self) -> usize {
        Arc::strong_count(&self.0)
    }

    #[cfg(test)]
    pub fn to_vec(&self) -> Vec<u8> {
        self.read().map(|b| b.to_vec()).unwrap_or_default()
    }

    /// Count this payload's RAM against `gauge` (once; the ring calls it on push).
    fn track(&self, gauge: &Arc<AtomicUsize>) {
        let state = self.0.state.lock().unwrap_or_else(|e| e.into_inner());
        if matches!(&*state, State::Ram(_)) && self.0.gauge.set(Arc::clone(gauge)).is_ok() {
            gauge.fetch_add(self.0.len, Relaxed);
        }
    }

    /// Move the bytes to `file` (spill thread). Returns false when they were already gone.
    fn spill_to(&self, file: &Arc<SpillFile>) -> Result<bool, String> {
        let bytes = match &*self.0.state.lock().unwrap_or_else(|e| e.into_inner()) {
            State::Ram(bytes) => Arc::clone(bytes),
            State::Disk { .. } => return Ok(false),
        };
        // Written outside the lock: a save reading this payload meanwhile still gets RAM.
        let at = file.append(&bytes)?;
        let mut state = self.0.state.lock().unwrap_or_else(|e| e.into_inner());
        *state = State::Disk { file: Arc::clone(file), at };
        if let Some(gauge) = self.0.gauge.get() {
            gauge.fetch_sub(self.0.len, Relaxed);
        }
        Ok(true)
    }
}

impl Drop for Chunk {
    fn drop(&mut self) {
        let in_ram = matches!(self.state.get_mut().unwrap_or_else(|e| e.into_inner()), State::Ram(_));
        if let (true, Some(gauge)) = (in_ram, self.gauge.get()) {
            gauge.fetch_sub(self.len, Relaxed);
        }
    }
}

impl From<Arc<[u8]>> for Payload {
    fn from(bytes: Arc<[u8]>) -> Self {
        Payload(Arc::new(Chunk { len: bytes.len(), state: Mutex::new(State::Ram(bytes)), gauge: OnceLock::new() }))
    }
}

impl From<&[u8]> for Payload {
    fn from(bytes: &[u8]) -> Self {
        Payload::from(Arc::<[u8]>::from(bytes))
    }
}

impl From<Vec<u8>> for Payload {
    fn from(bytes: Vec<u8>) -> Self {
        Payload::from(Arc::<[u8]>::from(bytes))
    }
}

/// A circular file of spilled payloads. Offsets are logical (they only grow); the
/// byte at logical offset `n` sits at `n % cap`, and stays valid until the writer has
/// reserved `n + cap`.
pub(crate) struct SpillFile {
    file: File,
    path: PathBuf,
    cap: u64,
    /// Logical end of everything the writer has started writing.
    reserved: AtomicU64,
}

impl SpillFile {
    /// Create `path` (delete-on-close, hidden) holding at most `cap` bytes.
    pub(crate) fn create(path: PathBuf, cap: u64) -> Result<Self, String> {
        let file = open_spill(&path).map_err(|e| format!("open spill file {}: {e}", path.display()))?;
        Ok(SpillFile { file, path, cap: cap.max(1), reserved: AtomicU64::new(0) })
    }

    pub(crate) fn path(&self) -> &std::path::Path {
        &self.path
    }

    pub(crate) fn cap(&self) -> u64 {
        self.cap
    }

    /// Append `bytes`; returns their logical offset. Only the spill thread writes.
    fn append(&self, bytes: &[u8]) -> Result<u64, String> {
        let len = bytes.len() as u64;
        if len > self.cap {
            return Err(format!("packet of {len} bytes is larger than the spill file"));
        }
        let at = self.reserved.load(Relaxed);
        // Reserve first: a reader of the bytes about to be overwritten sees them as lost.
        self.reserved.store(at + len, Release);
        let pos = at % self.cap;
        let first = (self.cap - pos).min(len) as usize;
        write_at(&self.file, &bytes[..first], pos).map_err(|e| format!("spill write: {e}"))?;
        if first < bytes.len() {
            write_at(&self.file, &bytes[first..], 0).map_err(|e| format!("spill write: {e}"))?;
        }
        Ok(at)
    }

    /// True while the bytes at logical `at` have not been overwritten.
    fn holds(&self, at: u64) -> bool {
        at >= self.reserved.load(Acquire).saturating_sub(self.cap)
    }

    fn read(&self, at: u64, len: usize) -> Result<Vec<u8>, String> {
        let lost = || format!("spilled packet at {at} was overwritten (spill file too small)");
        if !self.holds(at) {
            return Err(lost());
        }
        let mut out = vec![0u8; len];
        let pos = at % self.cap;
        let first = ((self.cap - pos) as usize).min(len);
        read_at(&self.file, &mut out[..first], pos).map_err(|e| format!("spill read: {e}"))?;
        if first < len {
            read_at(&self.file, &mut out[first..], 0).map_err(|e| format!("spill read: {e}"))?;
        }
        // The writer may have wrapped onto these bytes while they were read.
        if !self.holds(at) {
            return Err(lost());
        }
        Ok(out)
    }
}

#[cfg(windows)]
fn open_spill(path: &std::path::Path) -> std::io::Result<File> {
    use std::os::windows::fs::OpenOptionsExt;
    const FILE_ATTRIBUTE_HIDDEN: u32 = 0x2;
    const FILE_FLAG_DELETE_ON_CLOSE: u32 = 0x0400_0000;
    // FILE_SHARE_DELETE is required with delete-on-close; no other process opens it.
    const FILE_SHARE_DELETE: u32 = 0x4;
    std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(true)
        .share_mode(FILE_SHARE_DELETE)
        .attributes(FILE_ATTRIBUTE_HIDDEN)
        .custom_flags(FILE_FLAG_DELETE_ON_CLOSE)
        .open(path)
}

#[cfg(not(windows))]
fn open_spill(path: &std::path::Path) -> std::io::Result<File> {
    let file = std::fs::OpenOptions::new().read(true).write(true).create(true).truncate(true).open(path)?;
    let _ = std::fs::remove_file(path);
    Ok(file)
}

#[cfg(windows)]
fn write_at(file: &File, mut bytes: &[u8], mut pos: u64) -> std::io::Result<()> {
    use std::os::windows::fs::FileExt;
    while !bytes.is_empty() {
        let n = file.seek_write(bytes, pos)?;
        if n == 0 {
            return Err(std::io::ErrorKind::WriteZero.into());
        }
        bytes = &bytes[n..];
        pos += n as u64;
    }
    Ok(())
}

#[cfg(windows)]
fn read_at(file: &File, mut out: &mut [u8], mut pos: u64) -> std::io::Result<()> {
    use std::os::windows::fs::FileExt;
    while !out.is_empty() {
        let n = file.seek_read(out, pos)?;
        if n == 0 {
            return Err(std::io::ErrorKind::UnexpectedEof.into());
        }
        out = &mut out[n..];
        pos += n as u64;
    }
    Ok(())
}

#[cfg(not(windows))]
fn write_at(file: &File, bytes: &[u8], pos: u64) -> std::io::Result<()> {
    std::os::unix::fs::FileExt::write_all_at(file, bytes, pos)
}

#[cfg(not(windows))]
fn read_at(file: &File, out: &mut [u8], pos: u64) -> std::io::Result<()> {
    std::os::unix::fs::FileExt::read_exact_at(file, out, pos)
}

/// How the RAM budget is decided.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Budget {
    /// Installed RAM.
    pub total: u64,
    /// Bytes the audio rings take for the whole buffer.
    pub audio: u64,
    /// `RECALL_RAM_BUFFER_MB`, when set.
    pub forced: Option<u64>,
}

impl Budget {
    /// The steady budget: an eighth of RAM less the audio rings, at least 128 MiB.
    pub fn base(&self) -> u64 {
        if let Some(forced) = self.forced {
            return forced;
        }
        (self.total / 8).saturating_sub(self.audio).max(128 * MIB)
    }

    /// Free RAM below this means the game needs it more than the buffer does.
    pub fn floor(&self) -> u64 {
        (self.total / 10).max(3 * GIB / 2)
    }

    /// The budget now, given free RAM and what the ring holds in RAM.
    pub fn now(&self, avail: u64, ram: u64) -> u64 {
        let base = self.base();
        let floor = self.floor();
        if self.forced.is_some() || avail >= floor {
            return base;
        }
        // Give back what free RAM is short by, never below 32 MiB (about 10 s at 25 Mbps).
        base.min(ram.saturating_sub(floor - avail)).max(32 * MIB)
    }
}

/// What the spill side needs to size and place its file.
pub(crate) struct SpillPlan {
    /// The whole buffer at peak bitrate: the most a full spill can hold.
    pub want_file: u64,
}

/// The ring's RAM budget plus the spill thread. One per engine run; every ring the
/// encoder opens shares it.
pub struct Spill {
    budget: Budget,
    plan: SpillPlan,
    /// RAM held by ring payloads right now.
    gauge: Arc<AtomicUsize>,
    /// Bytes queued to the spill thread and not written yet.
    pending: Arc<AtomicUsize>,
    /// The budget as of the last free-RAM check.
    current: AtomicU64,
    checked: Mutex<Option<Instant>>,
    /// The spill thread, started on the first spill.
    worker: Mutex<Option<Sender<Payload>>>,
    /// Spill failed (no drive, or a write error): the ring drops old seconds instead.
    dead: Arc<AtomicBool>,
    file: Arc<OnceLock<Arc<SpillFile>>>,
    log: Arc<dyn Fn(&str) + Send + Sync>,
}

impl Spill {
    /// The production budget: installed RAM and `RECALL_RAM_BUFFER_MB`.
    pub fn from_system(buffer_secs: u32, bitrate: usize, audio_tracks: usize, log: Arc<dyn Fn(&str) + Send + Sync>) -> Self {
        let (total, _) = memory();
        let forced = std::env::var("RECALL_RAM_BUFFER_MB").ok().and_then(|v| v.trim().parse::<u64>().ok()).map(|mb| mb * MIB);
        // 48 kHz stereo f32 per track (audio.rs).
        let audio = audio_tracks as u64 * buffer_secs as u64 * 48_000 * 2 * 4;
        // Peak is 2x the VBR target (encoder.rs `set_max_bit_rate`).
        let want_file = buffer_secs as u64 * (bitrate as u64 * 2 / 8) + MIN_FILE;
        Self::new(Budget { total, audio, forced }, SpillPlan { want_file }, log)
    }

    pub(crate) fn new(budget: Budget, plan: SpillPlan, log: Arc<dyn Fn(&str) + Send + Sync>) -> Self {
        // Leftovers from a run that ended in a power cut or BSOD (delete-on-close never ran).
        // Not in unit tests: they must not touch this PC's drives.
        if !cfg!(test) {
            sweep::spawn(Arc::clone(&log));
        }
        log(&format!(
            "replay buffer: RAM budget {} MiB of {} MiB installed (audio rings {} MiB); older seconds spill to disk past it",
            budget.base() / MIB,
            budget.total / MIB,
            budget.audio / MIB
        ));
        Spill {
            current: AtomicU64::new(budget.base()),
            budget,
            plan,
            gauge: Arc::new(AtomicUsize::new(0)),
            pending: Arc::new(AtomicUsize::new(0)),
            checked: Mutex::new(None),
            worker: Mutex::new(None),
            dead: Arc::new(AtomicBool::new(false)),
            file: Arc::new(OnceLock::new()),
            log,
        }
    }

    /// RAM the ring's payloads hold.
    pub fn ram(&self) -> u64 {
        self.gauge.load(Relaxed) as u64
    }

    /// Bytes waiting for the spill thread.
    pub fn pending(&self) -> u64 {
        self.pending.load(Relaxed) as u64
    }

    pub fn alive(&self) -> bool {
        !self.dead.load(Relaxed)
    }

    /// Count a newly pushed payload's RAM.
    pub fn track(&self, payload: &Payload) {
        payload.track(&self.gauge);
    }

    /// The RAM budget, refreshed from free RAM at most every `RECHECK`.
    pub fn budget(&self) -> u64 {
        let mut checked = self.checked.lock().unwrap_or_else(|e| e.into_inner());
        if checked.map_or(true, |at| at.elapsed() >= RECHECK) {
            *checked = Some(Instant::now());
            let (_, avail) = memory();
            let now = self.budget.now(avail, self.ram());
            let before = self.current.swap(now, Relaxed);
            if now < before && now < self.budget.base() {
                (self.log)(&format!(
                    "replay buffer: free RAM {} MiB is under {} MiB; RAM budget down to {} MiB",
                    avail / MIB,
                    self.budget.floor() / MIB,
                    now / MIB
                ));
            } else if now > before && before < self.budget.base() && now == self.budget.base() {
                (self.log)("replay buffer: free RAM recovered; RAM budget back to normal");
            }
        }
        self.current.load(Relaxed)
    }

    /// Queue `payload` for the spill thread (started on the first call; it picks the
    /// drive itself, so a sleeping HDD never stalls the encoder). False when spilling
    /// is not possible.
    pub fn send(&self, payload: Payload) -> bool {
        if self.dead.load(Relaxed) {
            return false;
        }
        let mut worker = self.worker.lock().unwrap_or_else(|e| e.into_inner());
        if worker.is_none() {
            *worker = self.start();
        }
        let len = payload.len();
        self.pending.fetch_add(len, Relaxed);
        if worker.as_ref().is_some_and(|tx| tx.send(payload).is_ok()) {
            return true;
        }
        self.pending.fetch_sub(len, Relaxed);
        self.dead.store(true, Relaxed);
        false
    }

    fn start(&self) -> Option<Sender<Payload>> {
        let (tx, rx) = channel::<Payload>();
        let want = self.plan.want_file;
        let (pending, dead, log, slot) = (Arc::clone(&self.pending), Arc::clone(&self.dead), Arc::clone(&self.log), Arc::clone(&self.file));
        let spawned = std::thread::Builder::new().name("recall-spill".into()).spawn(move || {
            #[cfg(feature = "encode")]
            if let Err(e) = crate::encoder::enter_background() {
                log(&format!("spill thread priority: {e}"));
            }
            let file = match open_place(want) {
                Ok(file) => {
                    log(&format!(
                        "replay buffer: RAM budget full; spilling older seconds to {} ({} MiB max, {})",
                        file.0.path().display(),
                        file.0.cap() / MIB,
                        file.1
                    ));
                    let file = Arc::new(file.0);
                    let _ = slot.set(Arc::clone(&file));
                    Some(file)
                }
                Err(reason) => {
                    dead.store(true, Relaxed);
                    log(&format!("replay buffer: cannot spill to disk ({reason}); oldest seconds are dropped past the RAM budget"));
                    None
                }
            };
            for payload in rx {
                let len = payload.len();
                // Only the queue holds it (the ring already dropped it), or spilling is off.
                let result = match &file {
                    Some(file) if payload.holders() > 1 && !dead.load(Relaxed) => payload.spill_to(file),
                    _ => Ok(false),
                };
                pending.fetch_sub(len, Relaxed);
                if let Err(e) = result {
                    if !dead.swap(true, Relaxed) {
                        log(&format!("replay buffer: spill stopped ({e}); oldest seconds are dropped past the RAM budget"));
                    }
                }
            }
        });
        spawned.ok().map(|_| tx)
    }
}

/// Choose the drive and create the spill file there.
fn open_place(want: u64) -> Result<(SpillFile, String), String> {
    let game = crate::source::captured_exe_path();
    let place = drives::choose(want, KEEP_FREE, MIN_FILE, game.as_deref())?;
    std::fs::create_dir_all(&place.dir).map_err(|e| format!("create {}: {e}", place.dir.display()))?;
    let path = place.dir.join(format!("recall-replay-{}.spill", std::process::id()));
    Ok((SpillFile::create(path, place.cap)?, place.why))
}

/// (installed, free) physical RAM in bytes.
#[cfg(windows)]
pub(crate) fn memory() -> (u64, u64) {
    use windows::Win32::System::SystemInformation::{GlobalMemoryStatusEx, MEMORYSTATUSEX};
    let mut status = MEMORYSTATUSEX { dwLength: std::mem::size_of::<MEMORYSTATUSEX>() as u32, ..Default::default() };
    // SAFETY: dwLength is set; the call only fills the struct.
    match unsafe { GlobalMemoryStatusEx(&mut status) } {
        Ok(()) => (status.ullTotalPhys, status.ullAvailPhys),
        Err(_) => (8 * GIB, 4 * GIB),
    }
}

#[cfg(not(windows))]
pub(crate) fn memory() -> (u64, u64) {
    (8 * GIB, 4 * GIB)
}

#[cfg(test)]
mod tests;
