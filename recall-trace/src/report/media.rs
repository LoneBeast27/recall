//! Media a person adds to a report with the + button: screenshots (PNG, JPEG) and clips they
//! exported (MP4). Only these types, checked by their first bytes (MP4 by an allow-listed
//! brand, so MOV, HEIC, AVIF, 3GP, M4A and camera raw files are refused); each file and the
//! whole report have a size cap, and nothing is unpacked. Everything that can hold a place,
//! a time, a device or a name is removed: PNG keeps only its image chunks, JPEG loses its
//! APP1-15 and comment segments and anything after the image, MP4 has its metadata boxes
//! blanked and its creation times zeroed. Files are renamed `media-<n>.<ext>`.

/// The largest single file the + button takes.
pub const MAX_FILE: u64 = 20 * 1024 * 1024;
/// The largest report: GitHub takes attachments up to 25 MB.
pub const MAX_REPORT: u64 = 24 * 1024 * 1024;
/// The most files a report takes.
pub const MAX_MEDIA: usize = 6;

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

/// MP4 major brands a report takes (what video editors and Recall's own exports write).
const MP4_BRANDS: &[&[u8; 4]] = &[b"isom", b"iso2", b"iso4", b"iso5", b"iso6", b"mp41", b"mp42", b"avc1"];

/// What the first bytes say the file is (none: not a type a report takes).
pub fn sniff(head: &[u8]) -> Option<Kind> {
    if head.starts_with(&[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A]) {
        Some(Kind::Png)
    } else if head.starts_with(&[0xFF, 0xD8, 0xFF]) {
        Some(Kind::Jpeg)
    } else if head.len() >= 12 && &head[4..8] == b"ftyp" && MP4_BRANDS.iter().any(|b| &head[8..12] == *b) {
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

/// The file's bytes with every metadata field removed (none: damaged or not what it says).
pub fn clean(kind: Kind, data: Vec<u8>) -> Option<Vec<u8>> {
    match kind {
        Kind::Png => clean_png(&data),
        Kind::Jpeg => clean_jpeg(&data),
        Kind::Mp4 => clean_mp4(data),
    }
}

/// PNG: only the chunks that draw the picture.
fn clean_png(data: &[u8]) -> Option<Vec<u8>> {
    const KEEP: &[&[u8; 4]] = &[b"IHDR", b"PLTE", b"IDAT", b"IEND", b"tRNS", b"gAMA", b"cHRM", b"sRGB", b"pHYs"];
    let mut out = data.get(..8)?.to_vec();
    let mut at = 8;
    while at + 12 <= data.len() {
        let len = u32::from_be_bytes(data[at..at + 4].try_into().ok()?) as usize;
        let end = at.checked_add(12)?.checked_add(len)?;
        let chunk = data.get(at..end)?;
        let kind = &chunk[4..8];
        if KEEP.iter().any(|k| kind == *k) {
            out.extend_from_slice(chunk);
        }
        at = end;
        if kind == b"IEND" {
            return Some(out);
        }
    }
    None
}

/// JPEG: drop APP1..APP15 (EXIF, XMP, MPF and vendor data) and comments, and end the file at
/// the first end-of-image marker after the image data (a "motion photo" video, a second image
/// or anything else appended after it is cut off).
fn clean_jpeg(data: &[u8]) -> Option<Vec<u8>> {
    let mut out = vec![0xFF, 0xD8];
    let mut at = 2;
    loop {
        if *data.get(at)? != 0xFF {
            return None;
        }
        let marker = *data.get(at + 1)?;
        if marker == 0xDA {
            // Entropy-coded data never holds FF D9 (an FF there is followed by 00 or a restart
            // marker), so the first FF D9 from here is the image's end.
            let rest = &data[at..];
            let end = rest.windows(2).position(|w| w == [0xFF, 0xD9])?;
            out.extend_from_slice(&rest[..end + 2]);
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

/// MP4 boxes that only group other boxes (searched for metadata inside).
const CONTAINERS: &[&[u8; 4]] = &[b"moov", b"trak", b"mdia", b"minf", b"stbl", b"edts", b"dinf", b"mvex", b"moof", b"traf"];
/// Boxes that carry names, places, tools, cover art or vendor data: blanked to `free`.
const METADATA: &[&[u8; 4]] = &[b"udta", b"meta", b"uuid", b"ilst", b"XMP_", b"Xtra", b"loci", b"cprt"];
/// Top-level boxes a clip may have; any other is blanked too.
const TOP: &[&[u8; 4]] = &[b"ftyp", b"moov", b"mdat", b"free", b"skip", b"wide", b"moof", b"mfra", b"sidx", b"styp"];

/// MP4: every metadata box is overwritten in place by a `free` box of the same size (so the
/// sample offsets stay right), the creation and modification times in `mvhd`, `tkhd` and
/// `mdhd` are zeroed, and handler names in `hdlr` are blanked.
fn clean_mp4(mut data: Vec<u8>) -> Option<Vec<u8>> {
    let len = data.len();
    walk(&mut data, 0, len, true)?;
    Some(data)
}

fn walk(data: &mut [u8], from: usize, to: usize, top: bool) -> Option<()> {
    let mut at = from;
    while at + 8 <= to {
        let mut size = u32::from_be_bytes(data[at..at + 4].try_into().ok()?) as usize;
        let kind: [u8; 4] = data[at + 4..at + 8].try_into().ok()?;
        let mut header = 8;
        if size == 1 {
            size = usize::try_from(u64::from_be_bytes(data.get(at + 8..at + 16)?.try_into().ok()?)).ok()?;
            header = 16;
        } else if size == 0 {
            size = to - at; // to the end
        }
        let end = at.checked_add(size)?;
        if size < header || end > to {
            return None;
        }
        let blank = METADATA.iter().any(|m| kind == **m) || (top && !TOP.iter().any(|t| kind == **t));
        if blank {
            data[at + 4..at + 8].copy_from_slice(b"free");
            data[at + header..end].fill(0);
        } else if CONTAINERS.iter().any(|c| kind == **c) {
            walk(data, at + header, end, false)?;
        } else {
            match &kind {
                b"mvhd" | b"tkhd" | b"mdhd" => zero_times(&mut data[at + header..end]),
                b"hdlr" if end - at >= header + 24 => data[at + header + 24..end].fill(0), // the name
                _ => {}
            }
        }
        at = end;
    }
    Some(())
}

/// The creation and modification times at the start of a full box (version 0: 2 x 4 bytes,
/// version 1: 2 x 8 bytes, after the 4-byte version and flags).
fn zero_times(body: &mut [u8]) {
    let width = if body.first() == Some(&1) { 16 } else { 8 };
    if body.len() >= 4 + width {
        body[4..4 + width].fill(0);
    }
}
