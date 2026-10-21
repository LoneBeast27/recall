//! Library thumbnail tier on the SSD: `thumbs/<identity>.jpg`, ≈480 px.
//!
//! First sight of a clip imports the recorder's own thumbnail (`<folder>\thumbs\
//! <stem>-<epoch>-thumb.jpg`, 640×360) once — a plain copy when it is ≤ 640 px wide,
//! re-encoded to 480 px otherwise — or extracts a frame at 5% when there is none. Both run
//! on the clip's disk lane. After that `list_clips` finds the thumbnail from one listing of
//! `thumbs/`, so a warm launch reads no thumbnail from the library drive.
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::SystemTime;

use super::ffmpeg::{self, Seek};
use super::lane::{self, Priority};

pub const THUMB_WIDTH: u32 = 480;
const IMPORT_MAX_WIDTH: u32 = 640;

/// One listing of the thumbnail tier, for metadata-only lookups while listing clips.
pub struct ThumbIndex {
    dir: PathBuf,
    names: HashSet<String>,
}

impl ThumbIndex {
    pub fn lookup(&self, file_name: &str, bytes: u64, mtime_secs: u64) -> Option<PathBuf> {
        let name = format!("{}.jpg", super::identity(file_name, bytes, mtime_secs));
        self.names.contains(&name).then(|| self.dir.join(name))
    }
    pub fn len(&self) -> usize {
        self.names.len()
    }
}

pub fn thumb_index() -> ThumbIndex {
    index_in(&super::cache_root())
}

pub(super) fn index_in(root: &Path) -> ThumbIndex {
    let dir = root.join("thumbs");
    let names = std::fs::read_dir(&dir)
        .map(|listing| listing.flatten().map(|e| e.file_name().to_string_lossy().into_owned()).filter(|n| n.ends_with(".jpg")).collect())
        .unwrap_or_default();
    ThumbIndex { dir, names }
}

pub(super) fn thumb_path(root: &Path, id: &str) -> PathBuf {
    root.join("thumbs").join(format!("{id}.jpg"))
}

/// Cached thumbnail for `path` (stats the clip, reads nothing).
pub fn cached_thumb(path: &str) -> Option<String> {
    let (id, _, _) = super::identity_of(Path::new(path)).ok()?;
    let target = thumb_path(&super::cache_root(), &id);
    super::valid_file(&target).then(|| target.to_string_lossy().into_owned())
}

/// Cached, imported or generated thumbnail; misses run on the clip's disk lane at
/// `Visible` priority. Blocking.
pub fn ensure_thumb(path: &str) -> Result<String, String> {
    ensure_with(path, Priority::Visible)
}

pub(super) fn ensure_with(path: &str, priority: Priority) -> Result<String, String> {
    if let Some(hit) = cached_thumb(path) {
        return Ok(hit);
    }
    let root = super::cache_root();
    let source = PathBuf::from(path);
    lane::run_io(Path::new(path), priority, move || ensure_in(&root, &source))?
}

/// The work itself (call on the clip's lane, or anywhere for an SSD clip).
pub(super) fn ensure_in(root: &Path, source: &Path) -> Result<String, String> {
    let (id, _, _) = super::identity_of(source)?;
    let target = thumb_path(root, &id);
    let text = |path: &Path| path.to_string_lossy().into_owned();
    if super::valid_file(&target) {
        return Ok(text(&target));
    }
    std::fs::create_dir_all(target.parent().unwrap_or(root)).map_err(|error| format!("create thumbnail cache: {error}"))?;
    let lowered = super::drive_kind(source).is_hdd();
    let imported = recorder_thumb(source).map(|recorder| import(&recorder, &target, lowered));
    if let Some(Ok(())) = imported {
        return Ok(text(&target));
    }
    let duration = crate::services::library::mp4_duration(source).unwrap_or(0.0);
    let seconds = if duration.is_finite() && duration > 0.0 { duration * 0.05 } else { 0.0 };
    ffmpeg::extract_frame(source, seconds, THUMB_WIDTH, 4, &target, Seek::Fast, lowered)?;
    Ok(text(&target))
}

fn import(recorder: &Path, target: &Path, from_hdd: bool) -> Result<(), String> {
    let bytes = std::fs::read(recorder).map_err(|error| format!("read recorder thumbnail: {error}"))?;
    if from_hdd {
        lane::count_read(bytes.len() as u64);
    }
    let (width, _) = jpeg_size(&bytes).ok_or("recorder thumbnail is not a JPEG")?;
    if width <= IMPORT_MAX_WIDTH {
        return super::write_atomic(target, &bytes);
    }
    if let Ok(small) = resize_jpeg(&bytes, THUMB_WIDTH) {
        return super::write_atomic(target, &small);
    }
    // Decoder could not handle it: let ffmpeg try (one process per image, so only a fallback).
    let staged = super::temp_sibling(target).with_extension("src.jpg");
    std::fs::write(&staged, &bytes).map_err(|error| format!("stage recorder thumbnail: {error}"))?;
    let result = ffmpeg::scale_image(&staged, THUMB_WIDTH, target);
    let _ = std::fs::remove_file(&staged);
    result
}

/// Decodes, downsizes to `width` (aspect kept, even height) and re-encodes at quality 85,
/// in process: no helper spawn per thumbnail.
pub(super) fn resize_jpeg(bytes: &[u8], width: u32) -> Result<Vec<u8>, String> {
    let image = image::load_from_memory_with_format(bytes, image::ImageFormat::Jpeg).map_err(|error| error.to_string())?;
    if image.width() == 0 {
        return Err("empty image".into());
    }
    let height = ((u64::from(image.height()) * u64::from(width) / u64::from(image.width())) as u32 / 2 * 2).max(2);
    let small = image.thumbnail_exact(width, height).to_rgb8();
    let mut out = Vec::with_capacity(32 * 1024);
    image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, 85).encode_image(&small).map_err(|error| error.to_string())?;
    Ok(out)
}

/// `<stem>-<unix epoch>-thumb.jpg` → (stem, epoch).
pub fn recorder_thumb_parts(name: &str) -> Option<(&str, u64)> {
    let name = name.strip_suffix(".jpg")?.strip_suffix("-thumb")?;
    let (stem, epoch) = name.rsplit_once('-')?;
    if stem.is_empty() || epoch.len() < 9 || !epoch.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    Some((stem, epoch.parse().ok()?))
}

/// `<stem>-<unix epoch>-thumb.jpg` → stem.
pub fn recorder_thumb_stem(name: &str) -> Option<&str> {
    recorder_thumb_parts(name).map(|(stem, _)| stem)
}

struct FolderThumbs {
    /// The `thumbs` folder's modification time when listed (NTFS bumps it on every add).
    modified: Option<SystemTime>,
    by_stem: HashMap<String, (u64, PathBuf)>,
}

/// The newest recorder thumbnail for `clip`, from a cached listing of its `thumbs` folder.
/// Listed on first use; on a miss the folder is stat'ed and re-listed only if it changed
/// since (a thumbnail the recorder wrote after the last listing).
fn recorder_thumb(clip: &Path) -> Option<PathBuf> {
    static FOLDERS: OnceLock<Mutex<HashMap<PathBuf, FolderThumbs>>> = OnceLock::new();
    let folder = super::plain_path(clip.parent()?).join("thumbs");
    let stem = clip.file_stem()?.to_string_lossy().to_lowercase();
    let mut folders = FOLDERS.get_or_init(|| Mutex::new(HashMap::new())).lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    if let Some(hit) = folders.get(&folder).and_then(|f| f.by_stem.get(&stem)) {
        return Some(hit.1.clone());
    }
    let modified = folder.metadata().and_then(|m| m.modified()).ok();
    if folders.get(&folder).is_none_or(|f| f.modified != modified) {
        folders.insert(folder.clone(), FolderThumbs { modified, by_stem: list_recorder_thumbs(&folder) });
    }
    folders.get(&folder)?.by_stem.get(&stem).map(|(_, path)| path.clone())
}

fn list_recorder_thumbs(directory: &Path) -> HashMap<String, (u64, PathBuf)> {
    let mut thumbs: HashMap<String, (u64, PathBuf)> = HashMap::new();
    let Ok(listing) = std::fs::read_dir(directory) else { return thumbs };
    for entry in listing.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        let Some((stem, epoch)) = recorder_thumb_parts(&name) else { continue };
        let slot = thumbs.entry(stem.to_lowercase()).or_insert((0, PathBuf::new()));
        if epoch >= slot.0 {
            *slot = (epoch, entry.path());
        }
    }
    thumbs
}

/// (width, height) from the first SOFn segment of a JPEG.
pub(super) fn jpeg_size(bytes: &[u8]) -> Option<(u32, u32)> {
    if bytes.get(0..2)? != [0xFF, 0xD8] {
        return None;
    }
    let mut pos = 2;
    while pos + 4 <= bytes.len() {
        if bytes[pos] != 0xFF {
            return None;
        }
        let marker = bytes[pos + 1];
        if marker == 0xFF {
            pos += 1;
            continue;
        }
        if marker == 0x01 || (0xD0..=0xD7).contains(&marker) {
            pos += 2;
            continue;
        }
        let length = usize::from(u16::from_be_bytes([bytes[pos + 2], bytes[pos + 3]]));
        let is_sof = (0xC0..=0xCF).contains(&marker) && !matches!(marker, 0xC4 | 0xC8 | 0xCC);
        if is_sof {
            let segment = bytes.get(pos + 4..pos + 9)?;
            let height = u32::from(u16::from_be_bytes([segment[1], segment[2]]));
            let width = u32::from(u16::from_be_bytes([segment[3], segment[4]]));
            return Some((width, height));
        }
        if length < 2 {
            return None;
        }
        pos += 2 + length;
    }
    None
}
