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
use std::time::{SystemTime, UNIX_EPOCH};

use windows::Win32::Storage::FileSystem::{FILE_FLAG_NO_BUFFERING, FILE_FLAG_SEQUENTIAL_SCAN, FILE_FLAG_WRITE_THROUGH};

use super::budget::{self, INDEX_LOCK};
use super::lane::{self, Priority};
use super::evict::{self as lru, DEFAULT_CACHE_BUDGET};
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
    if bytes > budget / 2 || free < bytes.saturating_add(KEEP_FREE) {
        return Ok(ClipFill::direct(source, bytes, modified));
    }
    let directory = video_dir(&root);
    std::fs::create_dir_all(&directory).map_err(|error| format!("create fill folder: {error}"))?;
    remove_stale_parts(&directory);
    remove_left_fills(&directory, &id);
    budget::enforce(&root, budget, bytes);
    let target = directory.join(format!("{id}.mp4"));
    let part = super::temp_sibling(&target);
    let fill = ClipFill::create(source, &part, &target, bytes, modified, slow_drive_rate(), mega("RECALL_SLOW_DRIVE_FAIL"))?;
    fill.start(first)?;
    Ok(fill)
}

/// True when a clip this size stays out of the video tier.
pub fn too_large(bytes: u64) -> bool {
    bytes > budget_in(&super::cache_root()) / 2
}

fn video_dir(root: &Path) -> PathBuf {
    root.join("video")
}

pub(super) fn budget_in(root: &Path) -> u64 {
    lru::read_settings(root).cache_budget_bytes.unwrap_or(DEFAULT_CACHE_BUDGET)
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
    budget::enforce(root, budget, bytes);
    let free = super::drive_info(root).map_or(u64::MAX, |info| info.free);
    if free < bytes.saturating_add(KEEP_FREE) {
        return Err(format!("Not enough free space on the cache drive ({} free).", gib(free)));
    }
    let modified = metadata.modified().unwrap_or(UNIX_EPOCH);
    let temporary = super::temp_sibling(&target);
    COPY_DONE.store(0, Ordering::Relaxed);
    COPY_SIZE.store(bytes, Ordering::Relaxed);
    let copied = copy_sequential(source, &temporary, bytes, modified);
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
    Ok(target)
}

fn gib(bytes: u64) -> String {
    format!("{:.1} GB", bytes as f64 / 1e9)
}

fn touch(root: &Path, file: &str) {
    let _guard = INDEX_LOCK.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    let path = video_dir(root).join("lru.json");
    let mut index = lru::read_index(&path);
    index.insert(file.to_owned(), budget::now_ms());
    let _ = lru::write_index(&path, &index);
}

/// The part file an app closed mid-load left for this clip. A fill still running is not left.
fn remove_left_fills(directory: &Path, id: &str) {
    let prefix = format!("{id}.mp4.");
    let Ok(listing) = std::fs::read_dir(directory) else { return };
    for entry in listing.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.starts_with(&prefix) && name.ends_with(".part") && !super::fill::in_use(&entry.path()) {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

#[cfg(test)]
mod left_fills {
    use super::*;

    #[test]
    fn a_new_fill_clears_only_what_its_own_clip_left() {
        let dir = std::env::temp_dir().join(format!("recall-left-fills-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let source = dir.join("clip.mp4");
        std::fs::write(&source, vec![1u8; 8192]).unwrap();
        let modified = source.metadata().unwrap().modified().unwrap();
        let left = dir.join("aaaa.mp4.1-1.part");
        let other = dir.join("bbbb.mp4.1-2.part");
        std::fs::write(&left, b"left by a closed app").unwrap();
        std::fs::write(&other, b"another clip").unwrap();
        let running = ClipFill::create(&source, &dir.join("aaaa.mp4.1-3.part"), &dir.join("aaaa.mp4"), 8192, modified, 0, 0).unwrap();
        remove_left_fills(&dir, "aaaa");
        assert!(!left.exists(), "what a closed app left goes");
        assert!(other.exists(), "another clip's part stays");
        assert!(dir.join("aaaa.mp4.1-3.part").exists(), "a fill still running is not left");
        drop(running);
        let _ = std::fs::remove_dir_all(&dir);
    }
}

/// Interrupted copies older than an hour (a crash mid-copy).
fn remove_stale_parts(directory: &Path) {
    let Ok(listing) = std::fs::read_dir(directory) else { return };
    for entry in listing.flatten() {
        let stale = entry.file_name().to_string_lossy().ends_with(".part")
            && !super::fill::in_use(&entry.path())
            && entry.metadata().ok().and_then(|m| m.created().ok()).and_then(|t| t.elapsed().ok()).is_some_and(|age| age.as_secs() > STALE_PART_SECS);
        if stale {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

/// Unbuffered sequential copy, falling back to a buffered 8 MiB loop (e.g. a network share
/// that rejects unbuffered handles). The copy carries the source's modification time.
pub(super) fn copy_sequential(source: &Path, temporary: &Path, bytes: u64, modified: SystemTime) -> Result<(), String> {
    match copy_unbuffered(source, temporary, bytes, modified) {
        Ok(()) => Ok(()),
        Err(unbuffered) => {
            let _ = std::fs::remove_file(temporary);
            copy_buffered(source, temporary, bytes, modified).map_err(|buffered| format!("copy clip: {unbuffered}; {buffered}"))
        }
    }
}

pub(super) fn copy_unbuffered(source: &Path, temporary: &Path, bytes: u64, modified: SystemTime) -> Result<(), String> {
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
    }
    if copied < bytes {
        return Err("clip ended early".into());
    }
    output.set_len(bytes).map_err(|error| format!("trim copy: {error}"))?;
    output.set_modified(modified).map_err(|error| format!("stamp copy: {error}"))
}

fn copy_buffered(source: &Path, temporary: &Path, bytes: u64, modified: SystemTime) -> Result<(), String> {
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
    }
    if copied != bytes {
        return Err("clip size changed during copy".into());
    }
    output.set_modified(modified).map_err(|error| format!("stamp copy: {error}"))
}
