//! Video tier: whole-clip copies on the SSD (`video/<id>.mp4`) so preview, editor
//! preparation and export read the SSD instead of seeking the HDD. Copies are one
//! sequential pass of 8 MiB unbuffered reads (no system-cache pollution on a shared
//! desktop), published by atomic rename with the source's modification time. The tier is
//! capped at half the cache budget and is the first to be evicted (`budget`, `lru`).
use std::alloc::{Layout, alloc_zeroed, dealloc, handle_alloc_error};
use std::fs::OpenOptions;
use std::io::{Read, Write};
use std::os::windows::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use windows::Win32::Storage::FileSystem::{FILE_FLAG_NO_BUFFERING, FILE_FLAG_SEQUENTIAL_SCAN, FILE_FLAG_WRITE_THROUGH};

use super::budget::{self, INDEX_LOCK};
use super::lane::{self, Priority};
use super::evict::{self as lru, DEFAULT_CACHE_BUDGET};

const CHUNK: usize = 8 << 20;
const ALIGN: usize = 4096;
/// Free space the cache volume keeps after a copy.
const KEEP_FREE: u64 = 4 << 30;
const STALE_PART_SECS: u64 = 3600;

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
    if let Err(error) = copy_sequential(source, &temporary, bytes, modified) {
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

/// Interrupted copies older than an hour (a crash mid-copy).
fn remove_stale_parts(directory: &Path) {
    let Ok(listing) = std::fs::read_dir(directory) else { return };
    for entry in listing.flatten() {
        let stale = entry.file_name().to_string_lossy().ends_with(".part")
            && entry.metadata().ok().and_then(|m| m.created().ok()).and_then(|t| t.elapsed().ok()).is_some_and(|age| age.as_secs() > STALE_PART_SECS);
        if stale {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

/// 4 KiB-aligned heap buffer for unbuffered I/O.
struct AlignedBuffer {
    ptr: *mut u8,
    layout: Layout,
}

impl AlignedBuffer {
    fn new(len: usize) -> Self {
        let layout = Layout::from_size_align(len, ALIGN).expect("aligned layout");
        let ptr = unsafe { alloc_zeroed(layout) };
        if ptr.is_null() {
            handle_alloc_error(layout);
        }
        Self { ptr, layout }
    }
    fn bytes(&mut self) -> &mut [u8] {
        unsafe { std::slice::from_raw_parts_mut(self.ptr, self.layout.size()) }
    }
}

impl Drop for AlignedBuffer {
    fn drop(&mut self) {
        unsafe { dealloc(self.ptr, self.layout) };
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
        lane::count_read(read as u64);
    }
    if copied != bytes {
        return Err("clip size changed during copy".into());
    }
    output.set_modified(modified).map_err(|error| format!("stamp copy: {error}"))
}
