//! Media a person adds to a report with the + button: screenshots (PNG, JPEG) and clips they
//! exported (MP4). Only these types, checked by their first bytes, not their name; each file
//! and the whole report have a size cap, and nothing is unpacked. Picture metadata that can
//! hold a place, a device or a name (JPEG EXIF/XMP/comments, PNG text and EXIF chunks) is
//! removed; files are renamed `media-<n>.<ext>` so their own names stay out.

/// The largest single file the + button takes.
pub const MAX_FILE: u64 = 20 * 1024 * 1024;
/// The largest report: GitHub takes attachments up to 25 MB.
pub const MAX_REPORT: u64 = 24 * 1024 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Png,
    Jpeg,
    Mp4,
}

impl Kind {
    pub fn ext(self) -> &'static str {
        match self {
            Kind::Png => "png",
            Kind::Jpeg => "jpg",
            Kind::Mp4 => "mp4",
        }
    }
}

/// What the first bytes say the file is (none: not a type a report takes).
pub fn sniff(head: &[u8]) -> Option<Kind> {
    if head.starts_with(&[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A]) {
        Some(Kind::Png)
    } else if head.starts_with(&[0xFF, 0xD8, 0xFF]) {
        Some(Kind::Jpeg)
    } else if head.len() >= 12 && &head[4..8] == b"ftyp" {
        Some(Kind::Mp4)
    } else {
        None
    }
}

/// Why a file was refused, in words for the form.
pub fn refusal(len: u64, kind: Option<Kind>) -> Option<&'static str> {
    match kind {
        None => Some("Only PNG or JPEG screenshots and MP4 clips can be added."),
        Some(_) if len > MAX_FILE => Some("This file is over 20 MB. Export a shorter clip or a smaller screenshot."),
        Some(_) => None,
    }
}

/// The file's bytes with picture metadata removed. MP4 is kept as is (not parsed).
pub fn clean(kind: Kind, data: Vec<u8>) -> Option<Vec<u8>> {
    match kind {
        Kind::Png => clean_png(&data),
        Kind::Jpeg => clean_jpeg(&data),
        Kind::Mp4 => Some(data),
    }
}

/// PNG: keep every chunk but text (`tEXt`, `zTXt`, `iTXt`), `eXIf` and `tIME`.
fn clean_png(data: &[u8]) -> Option<Vec<u8>> {
    let mut out = data.get(..8)?.to_vec();
    let mut at = 8;
    while at + 12 <= data.len() {
        let len = u32::from_be_bytes(data[at..at + 4].try_into().ok()?) as usize;
        let end = at.checked_add(12)?.checked_add(len)?;
        let chunk = data.get(at..end)?;
        let kind = &chunk[4..8];
        if !matches!(kind, b"tEXt" | b"zTXt" | b"iTXt" | b"eXIf" | b"tIME") {
            out.extend_from_slice(chunk);
        }
        at = end;
        if kind == b"IEND" {
            return Some(out);
        }
    }
    None
}

/// JPEG: drop APP1..APP15 (EXIF, XMP and vendor data) and comments up to the image data.
fn clean_jpeg(data: &[u8]) -> Option<Vec<u8>> {
    let mut out = vec![0xFF, 0xD8];
    let mut at = 2;
    loop {
        if *data.get(at)? != 0xFF {
            return None;
        }
        let marker = *data.get(at + 1)?;
        if marker == 0xDA {
            // Start of scan: the rest is image data.
            out.extend_from_slice(&data[at..]);
            return Some(out);
        }
        let len = u16::from_be_bytes([*data.get(at + 2)?, *data.get(at + 3)?]) as usize;
        let end = at.checked_add(2)?.checked_add(len)?;
        let segment = data.get(at..end)?;
        let drop = (0xE1..=0xEF).contains(&marker) || marker == 0xFE;
        if !drop {
            out.extend_from_slice(segment);
        }
        at = end;
    }
}
