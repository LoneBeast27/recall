//! Video tier: whole-clip copies on the SSD (`video/<id>.mp4`) so preview, editor
//! preparation and export read the SSD instead of seeking the HDD. Copies are one
//! sequential pass of 8 MiB unbuffered reads (no system-cache pollution on a shared
//! desktop), published by atomic rename with the source's modification time. The tier is
//! capped at half the cache budget and is the first to be evicted (`budget`, `lru`).
use std::fs::OpenOptions;
use std::io::{Read, Write};
use std::os::windows::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use windows::Win32::Storage::FileSystem::{FILE_FLAG_NO_BUFFERING, FILE_FLAG_SEQUENTIAL_SCAN, FILE_FLAG_WRITE_THROUGH};

use super::budget::{self, INDEX_LOCK};
use super::lane::{self, Priority};
use super::evict as lru;
use super::fill::{AlignedBuffer, ClipFill};

const CHUNK: usize = 8 << 20;
const ALIGN: usize = 4096;
/// Free space the cache volume keeps after a copy.
const KEEP_FREE: u64 = 4 << 30;
const STALE_PART_SECS: u64 = 3600;

/// Bytes copied of the copy under way, and its size (0 when none): the open shows a percentage.
static COPY_DONE: AtomicU64 = AtomicU64::new(0);
static COPY_SIZE: AtomicU64 = AtomicU64::new(0);

/// Share of the whole-clip copy under way.
pub fn copy_progress() -> Option<f64> {
    let total = COPY_SIZE.load(Ordering::Relaxed);
    (total > 0).then(|| COPY_DONE.load(Ordering::Relaxed) as f64 / total as f64)
}

/// `RECALL_SLOW_DRIVE=<MB/s>` makes any drive act like a hard drive of that speed (the
/// slowed-drive check). Unset: 0, and only real hard drives count.
fn slow_drive_rate() -> u64 {
    mega("RECALL_SLOW_DRIVE")
}

fn mega(variable: &str) -> u64 {
    std::env::var(variable).ok().and_then(|v| v.parse::<u64>().ok()).unwrap_or(0).saturating_mul(1_000_000)
}

/// Clips here load progressively.
pub fn is_slow(path: &Path) -> bool {
    slow_drive_rate() > 0 || super::drive_kind(path).is_hdd()
}

/// Starts copying `source` to the SSD for progressive playback, index and head first. A clip
/// the video tier cannot take, by its size or for lack of room, is read where it lies: the
/// cache budget is never exceeded. `RECALL_SLOW_DRIVE_FAIL=<MB>` stops the drive after that much.
pub fn start_fill(source: &Path, metadata: &std::fs::Metadata, first: &[u64]) -> Result<ClipFill, String> {
    let root = super::cache_root();
    let bytes = metadata.len();
    let modified = metadata.modified().unwrap_or(UNIX_EPOCH);
    let name = super::file_name(source).ok_or("clip has no file name")?;
    let id = super::identity(&name, bytes, super::mtime_secs(metadata));
    let budget = budget_in(&root);
    let free = super::drive_info(&root).map_or(u64::MAX, |info| info.free);
    // A cache that overlaps a clip folder takes no new copies (`guard.rs`), nor one whose size
    // is unknown (storage.json unreadable, RI3-1): read in place.
    if bytes > budget / 2 || free < bytes.saturating_add(KEEP_FREE) || super::guard::blocked(&root).is_some() || budget_note(&root).is_some() {
        return Ok(ClipFill::direct(source, bytes, modified, mega("RECALL_SLOW_DRIVE_FAIL")));
    }
    // Listed before any file of it exists, so a crash's leftover part is still Recall's.
    super::owned::mark(&root, &format!("video/{id}.mp4"));
    let directory = video_dir(&root);
    std::fs::create_dir_all(&directory).map_err(|error| format!("create fill folder: {error}"))?;
    remove_stale_parts(&directory);
    remove_left_fills(&directory, &id);
    budget::enforce(&root, budget, bytes);
    let target = directory.join(format!("{id}.mp4"));
    let part = super::temp_sibling(&target);
    super::fill::claim(&part);
    // A head fetched by the filter probe starts the fill: index and start already on the SSD.
    let seeded = super::head::take(&root, &id, bytes, super::mtime_secs(metadata), &part).unwrap_or_default();
    let fill = ClipFill::create_seeded(source, &part, &target, bytes, modified, &seeded, slow_drive_rate(), mega("RECALL_SLOW_DRIVE_FAIL"))?;
    fill.start(first)?;
    Ok(fill)
}

/// True when a clip this size stays out of the video tier.
pub fn too_large(bytes: u64) -> bool {
    bytes > budget_in(&super::cache_root()) / 2
}

pub(super) fn video_dir(root: &Path) -> PathBuf {
    root.join("video")
}

/// The budget in `storage.json`. A first run (no budget set) picks one (`evict::default_budget`:
/// 2 GiB on the system drive, else from the cache drive's free space) and saves it; Settings
/// changes it from then on. A saved budget is never shrunk here. A file that is there but can't
/// be read (locked, or not JSON) is never written over: the budget read earlier this session
/// is used (else the pick, in memory only) and the next call reads the file again, so a passing
/// lock can't replace the user's budget. While it can't be read, eviction never goes below what
/// the cache already holds (`budget::enforce`) and Settings can't change the size.
pub(super) fn budget_in(root: &Path) -> u64 {
    let mut settings = match lru::load_settings(root) {
        lru::Stored::Read(settings) => settings,
        lru::Stored::Absent => lru::Settings::default(),
        lru::Stored::Unusable(why) | lru::Stored::Damaged(why) => {
            eprintln!("cache budget: {why}; using the last budget read (else the first-run pick) for now, file left as it is");
            return remembered(root).unwrap_or_else(|| first_run_budget(root));
        }
    };
    if let Some(budget) = settings.cache_budget_bytes {
        remember(root, budget);
        return budget;
    }
    let budget = first_run_budget(root);
    settings.cache_budget_bytes = Some(budget);
    settings.first_run = Some(true);
    if let Err(error) = lru::write_settings(root, &settings) {
        eprintln!("save first-run cache budget: {error}");
    }
    budget
}

/// The last budget read from each cache root's storage.json in this process (review RI2-2).
static LAST_READ: std::sync::Mutex<Vec<(PathBuf, u64)>> = std::sync::Mutex::new(Vec::new());

fn remember(root: &Path, budget: u64) {
    let mut last = LAST_READ.lock().unwrap_or_else(|p| p.into_inner());
    last.retain(|(r, _)| r != root);
    last.push((root.to_path_buf(), budget));
}

pub(super) fn remembered(root: &Path) -> Option<u64> {
    LAST_READ.lock().unwrap_or_else(|p| p.into_inner()).iter().find(|(r, _)| r == root).map(|(_, b)| *b)
}

/// Why the budget is not known right now: storage.json is there but locked, unreadable or not
/// JSON. None when it was read (or is absent: a first run).
pub(super) fn budget_note(root: &Path) -> Option<String> {
    match lru::load_settings(root) {
        lru::Stored::Unusable(why) | lru::Stored::Damaged(why) => Some(why),
        lru::Stored::Absent | lru::Stored::Read(_) => None,
    }
}

/// The budget a first run picks for the cache at `root`.
pub(super) fn first_run_budget(root: &Path) -> u64 {
    let (free, total) = super::drive_info(root).map_or((0, 0), |info| (info.free, info.total));
    lru::default_budget(free, total, super::drive::on_system_drive(root))
}

/// Applies the current budget (after a list load, an editor open, ...). Returns bytes freed.
pub fn enforce_budget() -> u64 {
    let root = super::cache_root();
    budget::enforce(&root, budget_in(&root), 0)
}

/// Complete SSD copy of `path`, if one exists (stats the source, no data read). Marks it used.
pub fn cached_video(path: &str) -> Option<String> {
    cached_in(&super::cache_root(), Path::new(path))
}

pub(super) fn cached_in(root: &Path, source: &Path) -> Option<String> {
    let (id, bytes, _) = super::identity_of(source).ok()?;
    let file = format!("{id}.mp4");
    let target = video_dir(root).join(&file);
    (target.metadata().ok()?.len() == bytes).then(|| {
        touch(root, &file);
        target.to_string_lossy().into_owned()
    })
}

/// Copies `path` into the video tier through its disk lane (`Interactive`) and waits.
/// Ok(copy); Ok(`path`) unchanged when the clip already lives on an SSD; Err when the clip
/// does not fit the budget or the cache drive's free space.
pub fn cache_video(path: &str) -> Result<String, String> {
    let source = PathBuf::from(path);
    if !super::drive_kind(&source).is_hdd() {
        return Ok(path.to_owned());
    }
    let root = super::cache_root();
    let lane_path = source.clone();
    lane::run_io(&lane_path, Priority::Interactive, move || {
        let budget = budget_in(&root);
        cache_in(&root, &source, budget).map(|copy| copy.to_string_lossy().into_owned())
    })?
}

/// The copy itself (call on the source's lane).
pub(super) fn cache_in(root: &Path, source: &Path, budget: u64) -> Result<PathBuf, String> {
    let room = |bytes| {
        budget::enforce(root, budget, bytes);
        true
    };
    copy_in(root, source, budget, &room, &FULL_SPEED)
}

/// The copy, after `room(bytes)` made room for it (false: no room, nothing copied).
pub(super) fn copy_in(root: &Path, source: &Path, budget: u64, room: &dyn Fn(u64) -> bool, pace: &Pace) -> Result<PathBuf, String> {
    if let Some(why) = super::guard::blocked(root) {
        return Err(format!("The SSD cache is paused. {why}"));
    }
    if let Some(why) = budget_note(root) {
        return Err(format!("The SSD cache takes no new copies while its size is unknown ({why})."));
    }
    let metadata = source.metadata().map_err(|error| format!("stat clip: {error}"))?;
    let bytes = metadata.len();
    let name = super::file_name(source).ok_or("clip has no file name")?;
    let file = format!("{}.mp4", super::identity(&name, bytes, super::mtime_secs(&metadata)));
    let directory = video_dir(root);
    std::fs::create_dir_all(&directory).map_err(|error| format!("create video cache: {error}"))?;
    let target = directory.join(&file);
    if target.metadata().is_ok_and(|m| m.len() == bytes) {
        touch(root, &file);
        return Ok(target);
    }
    let cap = budget / 2;
    if bytes > cap {
        return Err(format!("Clip ({}) is larger than the video cache allows ({}, half the cache size).", gib(bytes), gib(cap)));
    }
    remove_stale_parts(&directory);
    super::owned::mark(root, &format!("video/{file}"));
    if !room(bytes) {
        return Err("No room in the video cache without dropping a clip you are more likely to open.".into());
    }
    let free = super::drive_info(root).map_or(u64::MAX, |info| info.free);
    if free < bytes.saturating_add(KEEP_FREE) {
        return Err(format!("Not enough free space on the cache drive ({} free).", gib(free)));
    }
    let modified = metadata.modified().unwrap_or(UNIX_EPOCH);
    let temporary = super::temp_sibling(&target);
    COPY_DONE.store(0, Ordering::Relaxed);
    COPY_SIZE.store(bytes, Ordering::Relaxed);
    let copied = copy_sequential(source, &temporary, bytes, modified, pace);
    COPY_SIZE.store(0, Ordering::Relaxed);
    if let Err(error) = copied {
        let _ = std::fs::remove_file(&temporary);
        return Err(error);
    }
    let after = source.metadata().map_err(|error| format!("stat clip: {error}"))?;
    if after.len() != bytes || after.modified().ok() != metadata.modified().ok() {
        let _ = std::fs::remove_file(&temporary);
        return Err("Clip changed while it was being cached.".into());
    }
    super::ffmpeg::publish(&temporary, &target)?;
    touch(root, &file);
    super::head::forget(root, file.trim_end_matches(".mp4"));
    Ok(target)
}

fn gib(bytes: u64) -> String {
    format!("{:.1} GB", bytes as f64 / 1e9)
}

pub(super) fn touch(root: &Path, file: &str) {
    let _guard = INDEX_LOCK.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    let path = video_dir(root).join("lru.json");
    let mut index = lru::read_index(&path);
    index.insert(file.to_owned(), budget::now_ms());
    let _ = lru::write_index(&path, &index);
}

/// The part file an app closed mid-load left for this clip. A fill still running is not left,
/// and nor is anything Recall did not make (`owned.rs`).
fn remove_left_fills(directory: &Path, id: &str) {
    let Some(root) = directory.parent() else { return };
    let target = format!("{id}.mp4");
    let Ok(listing) = std::fs::read_dir(directory) else { return };
    for entry in listing.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if super::owned::temp_target(&name).as_deref() == Some(target.as_str()) && !super::fill::in_use(&entry.path()) {
            super::owned::remove(root, "video", &name);
        }
    }
}

#[cfg(test)]
mod left_fills {
    use super::*;

    #[test]
    fn a_new_fill_clears_only_what_its_own_clip_left() {
        let sandbox = crate::test_temp::TempDir::sandbox("recall-left-fills");
        let (root, dir) = (sandbox.join("cache"), sandbox.join("cache").join("video"));
        std::fs::create_dir_all(&dir).unwrap();
        let (a, b) = ("aaaaaaaaaaaaaaaa", "bbbbbbbbbbbbbbbb");
        super::super::owned::mark(&root, &format!("video/{a}.mp4"));
        super::super::owned::mark(&root, &format!("video/{b}.mp4"));
        let source = sandbox.join("clip.mp4");
        std::fs::write(&source, vec![1u8; 8192]).unwrap();
        let modified = source.metadata().unwrap().modified().unwrap();
        let left = dir.join(format!("{a}.mp4.1-1.part"));
        let other = dir.join(format!("{b}.mp4.1-2.part"));
        let user = dir.join(format!("{a}.mp4 my copy.part"));
        std::fs::write(&left, b"left by a closed app").unwrap();
        std::fs::write(&other, b"another clip").unwrap();
        std::fs::write(&user, b"not Recall's name").unwrap();
        let running = ClipFill::create(&source, &dir.join(format!("{a}.mp4.1-3.part")), &dir.join(format!("{a}.mp4")), 8192, modified, 0, 0).unwrap();
        remove_left_fills(&dir, a);
        assert!(!left.exists(), "what a closed app left goes");
        assert!(other.exists(), "another clip's part stays");
        assert!(user.exists(), "a name outside Recall's scheme stays");
        assert!(dir.join(format!("{a}.mp4.1-3.part")).exists(), "a fill still running is not left");
        // A copy held open by another handle (a video-tier copy in flight) is not left either.
        let copying = dir.join(format!("{a}.mp4.1-4.part"));
        let handle = std::fs::File::create(&copying).unwrap();
        remove_left_fills(&dir, a);
        assert!(copying.exists(), "an open copy is not swept");
        drop(handle);
        remove_left_fills(&dir, a);
        assert!(!copying.exists(), "closed and unowned, it goes");
        drop(running);
    }
}

/// Recall's interrupted copies older than an hour (a crash mid-copy): `<id>.mp4.<pid>-<nanos>.part`
/// of a listed copy (`owned.rs`). Any other `.part` (a user's, a long recording's) stays.
pub(super) fn remove_stale_parts(directory: &Path) {
    let Some(root) = directory.parent() else { return };
    let listed = super::owned::listed(root);
    let Ok(listing) = std::fs::read_dir(directory) else { return };
    for entry in listing.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        let stale = super::owned::temp_target(&name).is_some()
            && super::owned::owns(&listed, "video", &name)
            && !super::fill::in_use(&entry.path())
            && entry.metadata().ok().and_then(|m| m.created().ok()).and_then(|t| t.elapsed().ok()).is_some_and(|age| age.as_secs() > STALE_PART_SECS);
        if stale {
            super::owned::remove(root, "video", &name);
        }
    }
}

/// How a copy runs: at most `rate` bytes a second (0 = the drive's speed; the slowed-drive
/// check sets it), and stopping with [`YIELDED`] after any chunk where `yields` says so.
pub(super) struct Pace<'a> {
    pub rate: u64,
    pub yields: &'a dyn Fn() -> bool,
}

/// A warm-up copy stopped for on-demand work on its lane (it is tried again later).
pub(super) const YIELDED: &str = "the warm-up copy gave the drive to on-demand work";

fn never() -> bool {
    false
}

/// On-demand copies: full speed, never yield.
pub(super) const FULL_SPEED: Pace<'static> = Pace { rate: 0, yields: &never };

impl Pace<'_> {
    fn after_chunk(&self, started: Instant, copied: u64) -> Result<(), String> {
        if self.rate > 0 {
            let due = Duration::from_secs_f64(copied as f64 / self.rate as f64);
            if let Some(rest) = due.checked_sub(started.elapsed()) {
                std::thread::sleep(rest);
            }
        }
        if (self.yields)() { Err(YIELDED.into()) } else { Ok(()) }
    }
}

/// Unbuffered sequential copy, falling back to a buffered 8 MiB loop (e.g. a network share
/// that rejects unbuffered handles). The copy carries the source's modification time.
pub(super) fn copy_sequential(source: &Path, temporary: &Path, bytes: u64, modified: SystemTime, pace: &Pace) -> Result<(), String> {
    match copy_unbuffered(source, temporary, bytes, modified, pace) {
        Ok(()) => Ok(()),
        Err(yielded) if yielded == YIELDED => Err(yielded),
        Err(unbuffered) => {
            let _ = std::fs::remove_file(temporary);
            copy_buffered(source, temporary, bytes, modified, pace).map_err(|buffered| if buffered == YIELDED { buffered } else { format!("copy clip: {unbuffered}; {buffered}") })
        }
    }
}

pub(super) fn copy_unbuffered(source: &Path, temporary: &Path, bytes: u64, modified: SystemTime, pace: &Pace) -> Result<(), String> {
    let started = Instant::now();
    let mut input = OpenOptions::new()
        .read(true)
        .custom_flags(FILE_FLAG_NO_BUFFERING.0 | FILE_FLAG_SEQUENTIAL_SCAN.0)
        .open(source)
        .map_err(|error| format!("open clip: {error}"))?;
    let mut output = OpenOptions::new()
        .write(true)
        .create_new(true)
        .custom_flags(FILE_FLAG_NO_BUFFERING.0 | FILE_FLAG_WRITE_THROUGH.0)
        .open(temporary)
        .map_err(|error| format!("create copy: {error}"))?;
    let mut buffer = AlignedBuffer::new(CHUNK);
    let mut copied = 0u64;
    while copied < bytes {
        let read = input.read(buffer.bytes()).map_err(|error| format!("read clip: {error}"))?;
        if read == 0 {
            break;
        }
        if read % ALIGN != 0 && copied + (read as u64) < bytes {
            return Err("short unaligned read".into());
        }
        let padded = read.div_ceil(ALIGN) * ALIGN;
        buffer.bytes()[read..padded].fill(0);
        output.write_all(&buffer.bytes()[..padded]).map_err(|error| format!("write copy: {error}"))?;
        copied += read as u64;
        COPY_DONE.store(copied, Ordering::Relaxed);
        lane::count_read(read as u64);
        if copied < bytes {
            pace.after_chunk(started, copied)?;
        }
    }
    if copied < bytes {
        return Err("clip ended early".into());
    }
    output.set_len(bytes).map_err(|error| format!("trim copy: {error}"))?;
    output.set_modified(modified).map_err(|error| format!("stamp copy: {error}"))
}

fn copy_buffered(source: &Path, temporary: &Path, bytes: u64, modified: SystemTime, pace: &Pace) -> Result<(), String> {
    let started = Instant::now();
    let mut input = OpenOptions::new().read(true).custom_flags(FILE_FLAG_SEQUENTIAL_SCAN.0).open(source).map_err(|error| format!("open clip: {error}"))?;
    let mut output = OpenOptions::new().write(true).create_new(true).open(temporary).map_err(|error| format!("create copy: {error}"))?;
    let mut buffer = vec![0u8; CHUNK];
    let mut copied = 0u64;
    loop {
        let read = input.read(&mut buffer).map_err(|error| format!("read clip: {error}"))?;
        if read == 0 {
            break;
        }
        output.write_all(&buffer[..read]).map_err(|error| format!("write copy: {error}"))?;
        copied += read as u64;
        COPY_DONE.store(copied, Ordering::Relaxed);
        lane::count_read(read as u64);
        if copied < bytes {
            pace.after_chunk(started, copied)?;
        }
    }
    if copied != bytes {
        return Err("clip size changed during copy".into());
    }
    output.set_modified(modified).map_err(|error| format!("stamp copy: {error}"))
}
