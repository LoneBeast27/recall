//! Filter probe: when the Library shows one game, the heads of its most likely clips come to
//! the SSD while the hard drive is idle. For each of the top [`PROBE_TOP`] clips (score, then
//! newest) the index and the first [`HEAD_BYTES`] are copied, at the fill's chunk offsets, into
//! a sparse `heads/<id>.head` (chunk list in `heads/<id>.json`). Opening that clip later starts
//! its progressive fill from the head (`video::start_fill` → [`take`]): it plays at once and
//! the copy carries on from there.
//!
//! Each head is one Background task on the clip's disk lane, so an open (Interactive) always
//! goes first. A new filter cancels what has not started; nothing runs while the recorder
//! works. Heads of clips outside the current top are deleted at each probe, so at most
//! [`PROBE_TOP`] heads (~300 MB) exist, counted in the cache total.
use std::collections::HashSet;
use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use serde::{Deserialize, Serialize};

use super::fill::CHUNK;
use super::lane::Priority;

/// Clips of the filtered game whose heads are fetched.
pub const PROBE_TOP: usize = 8;
/// Bytes from the start of a clip in its head (plus its index).
pub const HEAD_BYTES: u64 = 32 << 20;

/// The current probe; a task of an older one does nothing.
static GENERATION: AtomicU64 = AtomicU64::new(0);

/// A listed clip of the filtered game.
#[derive(Debug, Clone, PartialEq)]
pub struct ProbeClip {
    pub path: String,
    pub bytes: u64,
    pub mtime_secs: u64,
}

impl ProbeClip {
    fn id(&self) -> String {
        let name = super::file_name(Path::new(&self.path)).unwrap_or_default();
        super::identity(&name, self.bytes, self.mtime_secs)
    }
}

#[derive(Debug, PartialEq, Serialize, Deserialize)]
struct HeadMeta {
    bytes: u64,
    mtime_secs: u64,
    chunks: Vec<u64>,
}

fn heads_dir(root: &Path) -> PathBuf {
    root.join("heads")
}

fn head_paths(root: &Path, id: &str) -> (PathBuf, PathBuf) {
    let dir = heads_dir(root);
    (dir.join(format!("{id}.head")), dir.join(format!("{id}.json")))
}

/// The top [`PROBE_TOP`] of `clips` by `score`, newest first among equals.
pub(super) fn pick(clips: &[ProbeClip], score: &dyn Fn(&ProbeClip) -> f64) -> Vec<ProbeClip> {
    let mut ranked: Vec<(f64, &ProbeClip)> = clips.iter().map(|clip| (score(clip), clip)).collect();
    ranked.sort_by(|a, b| b.0.total_cmp(&a.0).then(b.1.mtime_secs.cmp(&a.1.mtime_secs)).then_with(|| a.1.path.cmp(&b.1.path)));
    ranked.into_iter().take(PROBE_TOP).map(|(_, clip)| clip.clone()).collect()
}

/// The fill chunks a head holds: the index, then the start of the clip.
pub(super) fn head_chunks(bytes: u64, index: (u64, u64)) -> Vec<u64> {
    let last = bytes.div_ceil(CHUNK).max(1) - 1;
    let index_chunks = (index.0 / CHUNK).min(last)..=(index.1.saturating_sub(1) / CHUNK).min(last);
    let start_chunks = 0..HEAD_BYTES.div_ceil(CHUNK).min(last + 1);
    let mut chunks: Vec<u64> = index_chunks.chain(start_chunks).collect();
    chunks.sort_unstable();
    chunks.dedup();
    chunks
}

/// A new filter selects a game: fetch the heads of its likeliest clips. Returns the probe's
/// generation.
pub fn probe_filter(clips: Vec<ProbeClip>) -> u64 {
    let generation = GENERATION.fetch_add(1, Ordering::SeqCst) + 1;
    let root = super::cache_root();
    let picked = pick(&clips, &|clip| super::prefetch::score_of(&clip.id()));
    prune(&root, &picked.iter().map(ProbeClip::id).collect());
    for clip in picked {
        let root = root.clone();
        let source = PathBuf::from(&clip.path);
        super::submit_io(&source.clone(), Priority::Background, move || {
            if is_current(generation) && !super::recorder_busy() {
                if let Err(error) = write_head(&root, &source, &|| is_current(generation)) {
                    eprintln!("clip head {}: {error}", source.display());
                }
            }
        });
    }
    generation
}

/// The filter changed away from a game: queued heads are dropped.
pub fn cancel_probe() {
    GENERATION.fetch_add(1, Ordering::SeqCst);
}

pub(super) fn is_current(generation: u64) -> bool {
    GENERATION.load(Ordering::SeqCst) == generation
}

/// Deletes heads whose clip is not in `keep` (ids).
fn prune(root: &Path, keep: &HashSet<String>) {
    let Ok(listing) = std::fs::read_dir(heads_dir(root)) else { return };
    for entry in listing.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        let id = name.split('.').next().unwrap_or_default();
        let ours = super::owned::is_id(id) && [".head", ".json", ".head.part"].iter().any(|ext| name.len() == id.len() + ext.len() && name.ends_with(ext));
        if ours && !keep.contains(id) && super::guard::blocked(root).is_none() {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

/// Deletes the head of `id` (its clip now has a whole copy).
pub(super) fn forget(root: &Path, id: &str) {
    let (head, meta) = head_paths(root, id);
    let _ = std::fs::remove_file(head);
    let _ = std::fs::remove_file(meta);
}

/// Copies the head of `source` (call on its lane) for as long as `still` says yes. True when
/// a head was written; false when it already had one, a whole copy, or `still` said no.
pub(super) fn write_head(root: &Path, source: &Path, still: &dyn Fn() -> bool) -> Result<bool, String> {
    let metadata = source.metadata().map_err(|error| format!("stat: {error}"))?;
    let (bytes, mtime_secs) = (metadata.len(), super::mtime_secs(&metadata));
    let name = super::file_name(source).ok_or("no file name")?;
    let id = super::identity(&name, bytes, mtime_secs);
    let (head, meta_path) = head_paths(root, &id);
    let whole = super::video::video_dir(root).join(format!("{id}.mp4"));
    let headed = read_meta(&meta_path).is_some_and(|m| m.bytes == bytes && m.mtime_secs == mtime_secs) && head.is_file();
    if headed || whole.metadata().is_ok_and(|m| m.len() == bytes) {
        return Ok(false);
    }
    let mut input = File::open(source).map_err(|error| format!("open: {error}"))?;
    let index = crate::services::library::mp4_map::moov_range(&mut input).ok_or("clip has no index")?;
    let chunks = head_chunks(bytes, index);
    std::fs::create_dir_all(heads_dir(root)).map_err(|error| format!("create heads folder: {error}"))?;
    let part = head.with_extension("head.part");
    let copied = copy_chunks(&mut input, &part, bytes, &chunks, still);
    match copied {
        Ok(true) => {}
        other => {
            let _ = std::fs::remove_file(&part);
            return other;
        }
    }
    std::fs::rename(&part, &head).map_err(|error| format!("publish head: {error}"))?;
    let meta = serde_json::to_vec(&HeadMeta { bytes, mtime_secs, chunks }).map_err(|error| error.to_string())?;
    super::projects::write_atomic(&meta_path, &meta)?;
    Ok(true)
}

fn copy_chunks(input: &mut File, part: &Path, bytes: u64, chunks: &[u64], still: &dyn Fn() -> bool) -> Result<bool, String> {
    let mut output = OpenOptions::new().write(true).create(true).truncate(true).open(part).map_err(|error| format!("create head: {error}"))?;
    super::fill::make_sparse(&output)?;
    output.set_len(bytes).map_err(|error| format!("size head: {error}"))?;
    let mut buffer = vec![0u8; CHUNK as usize];
    for &chunk in chunks {
        if !still() {
            return Ok(false);
        }
        let offset = chunk * CHUNK;
        let length = (bytes - offset).min(CHUNK) as usize;
        input.seek(SeekFrom::Start(offset)).map_err(|error| format!("seek clip: {error}"))?;
        input.read_exact(&mut buffer[..length]).map_err(|error| format!("read clip: {error}"))?;
        output.seek(SeekFrom::Start(offset)).map_err(|error| format!("seek head: {error}"))?;
        output.write_all(&buffer[..length]).map_err(|error| format!("write head: {error}"))?;
        super::lane::count_read(length as u64);
    }
    // The fill reads this file unbuffered: everything is on disk before it is published.
    output.sync_all().map_err(|error| format!("flush head: {error}"))?;
    Ok(true)
}

fn read_meta(path: &Path) -> Option<HeadMeta> {
    std::fs::read(path).ok().and_then(|bytes| serde_json::from_slice(&bytes).ok())
}

/// Hands the head of clip `id` to a fill: moves it to `part` and returns its chunks. A head of
/// another revision of the clip is deleted. `None`: no head.
pub(super) fn take(root: &Path, id: &str, bytes: u64, mtime_secs: u64, part: &Path) -> Option<Vec<u64>> {
    let (head, meta_path) = head_paths(root, id);
    let meta = read_meta(&meta_path);
    let _ = std::fs::remove_file(&meta_path);
    let meta = meta.filter(|m| m.bytes == bytes && m.mtime_secs == mtime_secs && head.metadata().is_ok_and(|f| f.len() == bytes));
    match meta {
        Some(meta) if std::fs::rename(&head, part).is_ok() => Some(meta.chunks),
        _ => {
            let _ = std::fs::remove_file(&head);
            None
        }
    }
}

#[cfg(test)]
#[path = "head_tests.rs"]
mod tests;
