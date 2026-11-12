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
    const KEEP: &[&[u8; 4]] = &[b"IHDR", b"PLTE", b"IDAT", b"IEND", b"tRNS", b"gAMA", b"cHRM", b"sRGB", b"pHYs", b"cICP", b"mDCv", b"cLLi"];
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

/// JPEG: drop APP1..APP13 and APP15 (EXIF, XMP, MPF and vendor data) and comments (APP14,
/// Adobe's colour transform, stays: CMYK pictures need it), wherever they stand, also between
/// the scans of a progressive JPEG, and end the file at its end-of-image marker (a "motion
/// photo" video, a second image or anything else appended after it is cut off).
fn clean_jpeg(data: &[u8]) -> Option<Vec<u8>> {
    let mut out = vec![0xFF, 0xD8];
    let mut at = 2;
    loop {
        if *data.get(at)? != 0xFF {
            return None;
        }
        let marker = *data.get(at + 1)?;
        if marker == 0xD9 {
            out.extend_from_slice(&[0xFF, 0xD9]);
            return Some(out);
        }
        let len = u16::from_be_bytes([*data.get(at + 2)?, *data.get(at + 3)?]) as usize;
        let end = at.checked_add(2)?.checked_add(len)?;
        let segment = data.get(at..end)?;
        let drop = (0xE1..=0xED).contains(&marker) || marker == 0xEF || marker == 0xFE;
        if !drop {
            out.extend_from_slice(segment);
        }
        at = end;
        if marker == 0xDA {
            // Entropy-coded data: an FF in it is followed by 00 (a stuffed byte) or a restart
            // marker D0-D7; any other FF xx is the next marker.
            let start = at;
            loop {
                let b = *data.get(at)?;
                let next = data.get(at + 1).copied();
                if b == 0xFF && next.is_some_and(|n| n != 0x00 && !(0xD0..=0xD7).contains(&n)) {
                    break;
                }
                at += 1;
            }
            out.extend_from_slice(&data[start..at]);
        }
    }
}

/// MP4 boxes kept, by the box they sit in (`root` = the file itself). Every other box, at any
/// depth, is overwritten in place by a `free` box of the same size, so sample offsets stay
/// right; the contents of `free`, `skip` and `wide` boxes are zeroed too.
const ALLOWED: &[(&[u8; 4], &[&[u8; 4]])] = &[
    (b"root", &[b"ftyp", b"moov", b"mdat", b"moof", b"mfra", b"sidx", b"styp"]),
    (b"moov", &[b"mvhd", b"trak", b"mvex"]),
    (b"trak", &[b"tkhd", b"mdia", b"edts", b"tref"]),
    (b"edts", &[b"elst"]),
    (b"mdia", &[b"mdhd", b"hdlr", b"minf"]),
    (b"minf", &[b"vmhd", b"smhd", b"dinf", b"stbl"]),
    (b"dinf", &[b"dref"]),
    (b"stbl", &[b"stsd", b"stts", b"stss", b"ctts", b"stsc", b"stsz", b"stz2", b"stco", b"co64", b"sgpd", b"sbgp", b"sdtp", b"cslg", b"stps"]),
    (b"mvex", &[b"mehd", b"trex"]),
    (b"moof", &[b"mfhd", b"traf"]),
    (b"traf", &[b"tfhd", b"tfdt", b"trun", b"sgpd", b"sbgp", b"saiz", b"saio"]),
    (b"mfra", &[b"tfra", b"mfro"]),
];

/// Boxes whose children are boxes (walked); all other kept boxes are leaves.
const CONTAINERS: &[&[u8; 4]] = &[b"moov", b"trak", b"mdia", b"minf", b"stbl", b"edts", b"dinf", b"mvex", b"moof", b"traf", b"mfra"];

/// MP4: only the boxes on [`ALLOWED`] stay; the creation and modification times in `mvhd`,
/// `tkhd` and `mdhd` are zeroed and handler names in `hdlr` blanked. A file is refused when a
/// track is not video or sound (a GoPro or DJI GPS track, timecode, subtitles: their samples
/// sit in `mdat`, which is not rewritten), when a track has no handler, or when boxes nest
/// deeper than [`MAX_DEPTH`].
fn clean_mp4(mut data: Vec<u8>) -> Option<Vec<u8>> {
    let len = data.len();
    let mut tracks = Tracks::default();
    walk(&mut data, 0, len, *b"root", 0, &mut tracks)?;
    (tracks.seen == tracks.checked).then_some(data)
}

/// Boxes nested deeper than this make the file refused (no unbounded recursion).
const MAX_DEPTH: usize = 16;

/// Tracks found, and tracks whose handler was checked as video or sound.
#[derive(Default)]
struct Tracks {
    seen: usize,
    checked: usize,
}

fn walk(data: &mut [u8], from: usize, to: usize, parent: [u8; 4], depth: usize, tracks: &mut Tracks) -> Option<()> {
    if depth > MAX_DEPTH {
        return None;
    }
    let allowed = ALLOWED.iter().find(|(p, _)| **p == parent).map_or(&[][..], |(_, kids)| *kids);
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
        if !allowed.iter().any(|k| **k == kind) {
            // Not on the list (metadata, vendor data, padding): a zeroed `free` box.
            data[at + 4..at + 8].copy_from_slice(b"free");
            data[at + header..end].fill(0);
        } else if CONTAINERS.iter().any(|c| **c == kind) {
            if &kind == b"trak" {
                tracks.seen += 1;
            }
            walk(data, at + header, end, kind, depth + 1, tracks)?;
        } else {
            match &kind {
                b"mvhd" | b"tkhd" | b"mdhd" => zero_times(&mut data[at + header..end]),
                b"hdlr" => {
                    // A track's handler: only video and sound tracks are taken.
                    let handler = data.get(at + header + 8..at + header + 12)?;
                    if handler != b"vide" && handler != b"soun" {
                        return None;
                    }
                    tracks.checked += 1;
                    if end - at >= header + 24 {
                        data[at + header + 24..end].fill(0); // the name
                    }
                }
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
