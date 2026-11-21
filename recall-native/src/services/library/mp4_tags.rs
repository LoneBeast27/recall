//! File-level tags the recorder writes with FFmpeg's `use_metadata_tags`
//! (`recall/src/encoder/mux_index.rs`): `moov/udta/meta` (or `moov/meta`) holding `hdlr`
//! (`mdta`), `keys` (the tag names) and `ilst` (one item per key, its `data` the value).
//! Read in the app so a clip the engine prepared before opens without starting the engine.
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

use super::mp4::find_box;

/// Longest tag value read (the audio-sources list is the longest, well under this).
const MAX_VALUE: u64 = 64 << 10;
/// More keys than any file Recall writes.
const MAX_KEYS: u32 = 256;

/// The value of file tag `key`, None when the file has no such tag.
pub fn file_tag(path: &Path, key: &str) -> Option<String> {
    let mut file = std::fs::File::open(path).ok()?;
    file_tag_of(&mut file, key)
}

pub fn file_tag_of<R: Read + Seek>(reader: &mut R, key: &str) -> Option<String> {
    let end = reader.seek(SeekFrom::End(0)).ok()?;
    let moov = find_box(reader, 0, end, b"moov")?;
    let meta = find_box(reader, moov.0, moov.1, b"udta")
        .and_then(|udta| find_box(reader, udta.0, udta.1, b"meta"))
        .or_else(|| find_box(reader, moov.0, moov.1, b"meta"))?;
    let children = meta_children(reader, meta)?;
    let keys = find_box(reader, children, meta.1, b"keys")?;
    let index = key_index(reader, keys, key)?;
    let ilst = find_box(reader, children, meta.1, b"ilst")?;
    item_value(reader, ilst, index)
}

/// Where `meta`'s child boxes start: ISO `meta` is a full box (4 bytes of version and flags
/// first), QuickTime's is not.
fn meta_children<R: Read + Seek>(reader: &mut R, (start, end): (u64, u64)) -> Option<u64> {
    if end.saturating_sub(start) < 12 {
        return None;
    }
    reader.seek(SeekFrom::Start(start + 4)).ok()?;
    let mut kind = [0u8; 4];
    reader.read_exact(&mut kind).ok()?;
    // A child box at `start` has its type at `start + 4`; after version/flags, at `start + 8`.
    Some(if &kind == b"hdlr" || &kind == b"keys" || &kind == b"ilst" { start } else { start + 4 })
}

/// 1-based index of `key` among the `mdta` keys.
fn key_index<R: Read + Seek>(reader: &mut R, (start, end): (u64, u64), key: &str) -> Option<u32> {
    reader.seek(SeekFrom::Start(start + 4)).ok()?;
    let count = read_u32(reader)?.min(MAX_KEYS);
    let mut at = start + 8;
    for index in 1..=count {
        reader.seek(SeekFrom::Start(at)).ok()?;
        let size = u64::from(read_u32(reader)?);
        if size < 8 || at + size > end {
            return None;
        }
        let mut name = vec![0u8; size as usize];
        reader.read_exact(&mut name[4..]).ok()?;
        if &name[4..8] == b"mdta" && &name[8..] == key.as_bytes() {
            return Some(index);
        }
        at += size;
    }
    None
}

/// The text of `ilst` item `index` (its first `data` box).
fn item_value<R: Read + Seek>(reader: &mut R, (start, end): (u64, u64), index: u32) -> Option<String> {
    let item = find_box(reader, start, end, &index.to_be_bytes())?;
    let data = find_box(reader, item.0, item.1, b"data")?;
    // data payload: type (4), locale (4), then the value.
    let length = data.1.checked_sub(data.0 + 8)?.min(MAX_VALUE);
    reader.seek(SeekFrom::Start(data.0 + 8)).ok()?;
    let mut value = vec![0u8; length as usize];
    reader.read_exact(&mut value).ok()?;
    String::from_utf8(value).ok()
}

fn read_u32<R: Read>(reader: &mut R) -> Option<u32> {
    let mut value = [0u8; 4];
    reader.read_exact(&mut value).ok()?;
    Some(u32::from_be_bytes(value))
}

#[cfg(test)]
#[path = "mp4_tags_tests.rs"]
mod tests;
