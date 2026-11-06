//! Which files inside a cache unit are Recall's (`budget.rs` deletes only those): the
//! engine's editor files, `.jpg` frames, `<id>.jpg` thumbnails.
use std::path::{Path, PathBuf};

use super::budget::dir_size;
use super::owned::is_id;

fn entries(dir: &Path) -> Vec<(String, std::fs::Metadata, PathBuf)> {
    std::fs::read_dir(dir)
        .map(|listing| listing.flatten().filter_map(|e| Some((e.file_name().to_string_lossy().into_owned(), e.metadata().ok()?, e.path()))).collect())
        .unwrap_or_default()
}

/// Bytes freed by deleting the file at `path`.
pub(super) fn removed(path: &Path) -> u64 {
    let bytes = path.metadata().map_or(0, |m| m.len());
    if std::fs::remove_file(path).is_ok() { bytes } else { 0 }
}

/// A file's length, or a folder's whole size.
pub(super) fn size_of(metadata: &std::fs::Metadata, path: &Path) -> u64 {
    if metadata.is_dir() { dir_size(path).0 } else { metadata.len() }
}

/// `<id>.jpg`: a library thumbnail.
pub(super) fn is_thumb(name: &str) -> bool {
    name.strip_suffix(".jpg").is_some_and(is_id)
}

/// A poster, filmstrip or scrub frame (all `.jpg`).
pub(super) fn is_frame(name: &str) -> bool {
    name.to_ascii_lowercase().ends_with(".jpg")
}

/// What the engine writes into an editor folder (`recall/src/editor_media.rs`): its
/// `clip.json` (and the `.tmp` of a rewrite) and the audio stems.
fn is_engine_file(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    [".flac", ".wav", ".json", ".tmp"].iter().any(|ext| lower.ends_with(ext))
}

/// An engine stem folder: a 16-hex key holding its `clip.json`, or only engine files (a
/// preparation cut short before the manifest).
pub(super) fn engine_folder(name: &str, path: &Path) -> bool {
    is_id(name) && (path.join("clip.json").is_file() || entries(path).iter().all(|(file, meta, _)| meta.is_file() && is_engine_file(file)))
}

/// Bytes in an editor folder that are not the engine's (files and subfolders).
pub(super) fn foreign_bytes(dir: &Path) -> u64 {
    entries(dir).iter().filter(|(name, meta, _)| !(meta.is_file() && is_engine_file(name))).map(|(_, meta, path)| size_of(meta, path)).sum()
}

/// Deletes the engine's files in an editor folder, then the folder once empty. Anything else
/// in it stays (and with it the folder). Returns bytes freed.
pub(super) fn remove_engine_folder(dir: &Path) -> u64 {
    let freed = entries(dir).iter().filter(|(name, meta, _)| meta.is_file() && is_engine_file(name)).map(|(_, _, path)| removed(path)).sum();
    let _ = std::fs::remove_dir(dir);
    freed
}
