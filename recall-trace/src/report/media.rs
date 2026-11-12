//! Files a person adds to a report (the + button or a drop on the Report page): screenshots
//! (PNG, JPEG), clips (MP4), sound (WAV, MP3, M4A) and plain text (.txt, .log). Types are
//! told by their content, not their name (MP4 and M4A by an allow-listed brand, so MOV, HEIC,
//! AVIF, 3GP and camera raw files are refused; text must be UTF-8 without binary bytes); each
//! file and the whole report have a size cap, and nothing is unpacked. Metadata is removed:
//! PNG keeps only its image chunks, JPEG loses its APP1-15 and comment segments and anything
//! after the image, MP4 and M4A have their metadata boxes blanked and creation times zeroed,
//! WAV keeps only its fmt, fact and data chunks, MP3 keeps only its audio frames. Text is sent
//! exactly as written. Files are renamed `media-<n>.<ext>`.

/// The largest single file the + button takes.
pub const MAX_FILE: u64 = 20 * 1024 * 1024;
/// The largest report: GitHub takes attachments up to 25 MB.
pub const MAX_REPORT: u64 = 24 * 1024 * 1024;
/// The most files a report takes.
pub const MAX_MEDIA: usize = 6;
/// The largest text file a report takes.
pub const MAX_TEXT: u64 = 1024 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Png,
    Jpeg,
    Mp4,
    M4a,
    Wav,
    Mp3,
    Txt,
    Log,
}

impl Kind {
    pub fn ext(self) -> &'static str {
        match self {
            Kind::Png => "png",
            Kind::Jpeg => "jpg",
            Kind::Mp4 => "mp4",
            Kind::M4a => "m4a",
            Kind::Wav => "wav",
            Kind::Mp3 => "mp3",
            Kind::Txt => "txt",
            Kind::Log => "log",
        }
    }

    /// The largest file of this kind a report takes.
    pub fn cap(self) -> u64 {
        if matches!(self, Kind::Txt | Kind::Log) { MAX_TEXT } else { MAX_FILE }
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
    } else if head.len() >= 12 && &head[4..8] == b"ftyp" && &head[8..12] == b"M4A " {
        Some(Kind::M4a)
    } else if head.len() >= 12 && &head[0..4] == b"RIFF" && &head[8..12] == b"WAVE" {
        Some(Kind::Wav)
    } else if head.starts_with(b"ID3") || (head.len() >= 4 && mp3_frame_len(&head[..4]).is_some()) {
        Some(Kind::Mp3)
    } else {
        None
    }
}

/// A text file: `.txt` or `.log` by name, and by content UTF-8 with no NUL or other control
/// bytes than tab, line breaks and form feed.
pub fn text_kind(name_ext: &str, data: &[u8]) -> Option<Kind> {
    let kind = match name_ext.to_ascii_lowercase().as_str() {
        "txt" => Kind::Txt,
        "log" => Kind::Log,
        _ => return None,
    };
    let text = std::str::from_utf8(data).ok()?;
    text.chars().all(|c| !c.is_control() || matches!(c, '\t' | '\n' | '\r' | '\u{c}' | '\u{1b}')).then_some(kind)
}

/// What the form says about a file of another type.
pub const REFUSED: &str = "Only PNG or JPEG screenshots, MP4 clips, WAV, MP3 or M4A sound and .txt or .log text can be added.";

/// What the form says when a file of `kind` cannot be cleaned.
pub fn damaged(kind: Kind) -> &'static str {
    match kind {
        Kind::Mp4 => "This clip is damaged or carries extra data such as a GPS track. Export it again from Recall or an editor.",
        Kind::M4a => "This sound file is damaged or carries extra tracks. Export it again.",
        Kind::Wav => "This WAV file is damaged or not a plain recording.",
        Kind::Mp3 => "This MP3 file holds no sound Recall can read.",
        Kind::Png | Kind::Jpeg => "This picture is damaged.",
        Kind::Txt | Kind::Log => "This text file holds bytes that are not plain text.",
    }
}

/// Why a file was refused, in words for the form.
pub fn refusal(len: u64, kind: Option<Kind>) -> Option<&'static str> {
    match kind {
        None => Some(REFUSED),
        Some(kind) if len > kind.cap() && kind.cap() == MAX_TEXT => Some("This text file is over 1 MB. Add the part that shows the problem."),
        Some(_) if len > MAX_FILE => Some("This file is over 20 MB. Export a shorter clip or a smaller screenshot."),
        Some(_) => None,
    }
}

/// The file's bytes with every metadata field removed (none: damaged or not what it says).
pub fn clean(kind: Kind, data: Vec<u8>) -> Option<Vec<u8>> {
    match kind {
        Kind::Png => clean_png(&data),
        Kind::Jpeg => clean_jpeg(&data),
        Kind::Mp4 | Kind::M4a => clean_mp4(data),
        Kind::Wav => clean_wav(&data),
        Kind::Mp3 => clean_mp3(&data),
        Kind::Txt | Kind::Log => Some(data),
    }
}

/// WAV: a new RIFF file holding only the `fmt `, `fact` and `data` chunks (LIST/INFO, bext,
/// iXML, id3 and every other chunk dropped). Refused without `fmt ` and `data`, or as RF64.
fn clean_wav(data: &[u8]) -> Option<Vec<u8>> {
    if data.get(0..4)? != b"RIFF" || data.get(8..12)? != b"WAVE" {
        return None;
    }
    let mut body = b"WAVE".to_vec();
    let (mut fmt, mut sound) = (false, false);
    let mut at = 12;
    while at + 8 <= data.len() {
        let id: [u8; 4] = data[at..at + 4].try_into().ok()?;
        let len = u32::from_le_bytes(data[at + 4..at + 8].try_into().ok()?) as usize;
        let end = at.checked_add(8)?.checked_add(len)?;
        let chunk = data.get(at..end)?;
        // Only the first fmt (at most 40 bytes: WAVE_FORMAT_EXTENSIBLE) and data chunks, and
        // fact only at its 4 bytes: a longer or second chunk could carry any bytes.
        let keep = match &id {
            b"fmt " => !fmt && len <= 40,
            b"data" => !sound,
            b"fact" => len == 4,
            _ => false,
        };
        if &id == b"fmt " && len > 40 {
            return None;
        }
        if keep {
            body.extend_from_slice(chunk);
            if len % 2 == 1 {
                body.push(0);
            }
            fmt |= &id == b"fmt ";
            sound |= &id == b"data";
        }
        at = end + len % 2;
    }
    if !(fmt && sound) {
        return None;
    }
    let mut out = b"RIFF".to_vec();
    out.extend(u32::try_from(body.len()).ok()?.to_le_bytes());
    out.extend(body);
    Some(out)
}

/// MP3: the audio frames only. ID3v2 tags at the start, ID3v1 and APE tags at the end, and any
/// other bytes that are not a run of valid frames are dropped. Refused without a frame.
fn clean_mp3(data: &[u8]) -> Option<Vec<u8>> {
    let mut at = 0;
    // ID3v2 tags (there may be more than one): 10-byte header, syncsafe size, optional footer.
    while data.get(at..at + 3) == Some(b"ID3") {
        let flags = *data.get(at + 5)?;
        let size = data.get(at + 6..at + 10)?.iter().fold(0usize, |s, b| (s << 7) | usize::from(b & 0x7F));
        at = at.checked_add(10 + size + if flags & 0x10 != 0 { 10 } else { 0 })?;
    }
    let end = mp3_audio_end(data, at);
    let data = &data[..end];
    let mut out = Vec::with_capacity(end.saturating_sub(at));
    let mut in_sync = false;
    while at + 4 <= data.len() {
        match mp3_frame_len(&data[at..at + 4]) {
            // A whole frame: kept when it follows another frame, or when the next frame (or the
            // end) comes right after it, so a stray FF Ex in junk is not taken for a frame.
            Some(len) if at + len <= data.len() && (in_sync || at + len == data.len() || data.get(at + len..at + len + 4).and_then(mp3_frame_len).is_some()) => {
                out.extend_from_slice(&data[at..at + len]);
                at += len;
                in_sync = true;
            }
            // The last frame is cut short: the whole frames before it are kept.
            Some(len) if at + len > data.len() && in_sync => break,
            // Not a frame here: step one byte forward and look again (linear, bounded).
            _ => {
                at += 1;
                in_sync = false;
            }
        }
    }
    (!out.is_empty()).then_some(out)
}

/// Where the audio ends: before an ID3v1 tag (`TAG`, the last 128 bytes) and an APE tag (its
/// `APETAGEX` footer gives its size) at the end of the file.
fn mp3_audio_end(data: &[u8], start: usize) -> usize {
    let mut end = data.len();
    if end >= start + 128 && &data[end - 128..end - 125] == b"TAG" {
        end -= 128;
    }
    if end >= start + 32 && &data[end - 32..end - 24] == b"APETAGEX" {
        let size = u32::from_le_bytes(data[end - 20..end - 16].try_into().unwrap_or([0; 4])) as usize;
        let flags = u32::from_le_bytes(data[end - 12..end - 8].try_into().unwrap_or([0; 4]));
        let whole = size + if flags & 0x8000_0000 != 0 { 32 } else { 0 };
        end = end.saturating_sub(whole).max(start);
    }
    end
}

/// The length of the MPEG audio Layer III frame whose 4-byte header is `h` (none: not one).
fn mp3_frame_len(h: &[u8]) -> Option<usize> {
    if h.len() < 4 || h[0] != 0xFF || h[1] & 0xE0 != 0xE0 {
        return None;
    }
    let version = (h[1] >> 3) & 3; // 0 = 2.5, 2 = 2, 3 = 1
    let layer = (h[1] >> 1) & 3; // 1 = Layer III
    let bitrate_index = usize::from(h[2] >> 4);
    let rate_index = usize::from((h[2] >> 2) & 3);
    if version == 1 || layer != 1 || bitrate_index == 0 || bitrate_index == 15 || rate_index == 3 {
        return None;
    }
    const MPEG1: [usize; 15] = [0, 32, 40, 48, 56, 64, 80, 96, 112, 128, 160, 192, 224, 256, 320];
    const MPEG2: [usize; 15] = [0, 8, 16, 24, 32, 40, 48, 56, 64, 80, 96, 112, 128, 144, 160];
    let (kbps, rate, factor) = match version {
        3 => (MPEG1[bitrate_index], [44100, 48000, 32000][rate_index], 144),
        2 => (MPEG2[bitrate_index], [22050, 24000, 16000][rate_index], 72),
        _ => (MPEG2[bitrate_index], [11025, 12000, 8000][rate_index], 72),
    };
    let padding = usize::from((h[2] >> 1) & 1);
    let len = factor * kbps * 1000 / rate + padding;
    (len > 4).then_some(len)
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
        // Fill bytes: any number of FF may stand before a marker.
        while data.get(at + 1) == Some(&0xFF) {
            at += 1;
        }
        let marker = *data.get(at + 1)?;
        if marker == 0xD9 {
            out.extend_from_slice(&[0xFF, 0xD9]);
            return Some(out);
        }
        let len = u16::from_be_bytes([*data.get(at + 2)?, *data.get(at + 3)?]) as usize;
        let end = at.checked_add(2)?.checked_add(len)?;
        let segment = data.get(at..end)?;
        // APP1-13, APP15, comments and the JPGn/reserved markers F0-FD are dropped. APP0 is
        // rebuilt as a plain JFIF header (no thumbnail); APP14 (Adobe, CMYK) is kept only at
        // its fixed 14-byte length.
        let drop = (0xE1..=0xED).contains(&marker) || marker == 0xEF || marker == 0xFE || (0xF0..=0xFD).contains(&marker);
        if marker == 0xE0 {
            out.extend_from_slice(&JFIF);
        } else if marker == 0xEE {
            if len == 14 && segment.get(4..9) == Some(b"Adobe") {
                out.extend_from_slice(segment);
            }
        } else if !drop {
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

/// A plain JFIF APP0: version 1.1, no density units, 1:1, no thumbnail.
const JFIF: [u8; 18] = [0xFF, 0xE0, 0x00, 0x10, b'J', b'F', b'I', b'F', 0x00, 0x01, 0x01, 0x00, 0x00, 0x01, 0x00, 0x01, 0x00, 0x00];

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

/// The box at `at` (inside `..to`): its type, header length and end.
fn box_at(data: &[u8], at: usize, to: usize) -> Option<([u8; 4], usize, usize)> {
    let mut size = u32::from_be_bytes(data.get(at..at + 4)?.try_into().ok()?) as usize;
    let kind: [u8; 4] = data.get(at + 4..at + 8)?.try_into().ok()?;
    let mut header = 8;
    if size == 1 {
        size = usize::try_from(u64::from_be_bytes(data.get(at + 8..at + 16)?.try_into().ok()?)).ok()?;
        header = 16;
    } else if size == 0 {
        size = to - at; // to the end
    }
    let end = at.checked_add(size)?;
    (size >= header && end <= to).then_some((kind, header, end))
}

/// Overwrite the box at `at..end` (header `header` bytes) with a zeroed `free` box.
fn blank(data: &mut [u8], at: usize, header: usize, end: usize) {
    data[at + 4..at + 8].copy_from_slice(b"free");
    data[at + header..end].fill(0);
}

fn walk(data: &mut [u8], from: usize, to: usize, parent: [u8; 4], depth: usize, tracks: &mut Tracks) -> Option<()> {
    if depth > MAX_DEPTH {
        return None;
    }
    let allowed = ALLOWED.iter().find(|(p, _)| **p == parent).map_or(&[][..], |(_, kids)| *kids);
    let mut at = from;
    while at + 8 <= to {
        let (kind, header, end) = box_at(data, at, to)?;
        if !allowed.iter().any(|k| **k == kind) {
            // Not on the list (metadata, vendor data, padding): a zeroed `free` box.
            blank(data, at, header, end);
        } else if CONTAINERS.iter().any(|c| **c == kind) {
            let handlers = tracks.checked;
            walk(data, at + header, end, kind, depth + 1, tracks)?;
            if &kind == b"trak" {
                tracks.seen += 1;
                // Each track has exactly one video or sound handler of its own.
                if tracks.checked != handlers + 1 {
                    return None;
                }
            }
        } else {
            match &kind {
                b"mvhd" | b"tkhd" | b"mdhd" => zero_times(&mut data[at + header..end]),
                b"stsd" => clean_stsd(data, at + header, end)?,
                b"dref" => self_contained(data, at + header, end)?,
                b"hdlr" => {
                    // A track's handler: only video and sound tracks are taken.
                    // Read inside this box only: a short hdlr must not borrow the next box's bytes.
                    if end < at + header + 12 {
                        return None;
                    }
                    let handler = &data[at + header + 8..at + header + 12];
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
    data[at.min(to)..to].fill(0); // bytes too few to be a box
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

/// Video sample entries: a 78-byte fixed part (its encoder name at 42..74), then child boxes.
const VIDEO_ENTRIES: &[&[u8; 4]] = &[b"avc1", b"avc3", b"hvc1", b"hev1", b"av01", b"vp09"];
/// Sound sample entries (version 0): a 28-byte fixed part, then child boxes.
const SOUND_ENTRIES: &[&[u8; 4]] = &[b"mp4a", b"alac", b"Opus", b"fLaC", b"ac-3", b"ec-3"];
/// Child boxes of a sample entry that only describe the coding; any other (vendor `uuid`,
/// `udta`, `meta`, an ICC profile) becomes a zeroed `free` box.
const ENTRY_CHILDREN: &[&[u8; 4]] = &[b"avcC", b"hvcC", b"av1C", b"vpcC", b"esds", b"dOps", b"dfLa", b"dac3", b"dec3", b"alac", b"btrt", b"pasp", b"colr", b"clap", b"fiel", b"mdcv", b"clli", b"dvcC", b"dvvC"];

/// `stsd`: every sample entry must be a known video or sound coding; its encoder name is
/// zeroed and its children kept only from [`ENTRY_CHILDREN`] (a `colr` only as plain colour
/// numbers, `nclx`/`nclc`, never an ICC profile). Anything else refuses the file.
fn clean_stsd(data: &mut [u8], from: usize, to: usize) -> Option<()> {
    let mut at = from.checked_add(8)?; // version, flags, entry count
    while at + 8 <= to {
        let (kind, header, end) = box_at(data, at, to)?;
        let fixed = if VIDEO_ENTRIES.iter().any(|k| **k == kind) {
            if end < at + header + 78 {
                return None; // shorter than a video entry's fixed part
            }
            data[at + header + 42..at + header + 74].fill(0); // compressorname
            78
        } else if SOUND_ENTRIES.iter().any(|k| **k == kind) {
            if end < at + header + 28 {
                return None;
            }
            let version = u16::from_be_bytes([data[at + header + 8], data[at + header + 9]]);
            if version != 0 {
                return None; // QuickTime sound layouts carry more fields
            }
            28
        } else {
            return None;
        };
        let mut child = at.checked_add(header + fixed)?;
        while child + 8 <= end {
            let (kind, header, child_end) = box_at(data, child, end)?;
            let colour_numbers = &kind == b"colr" && matches!(data.get(child + header..child + header + 4), Some(b"nclx" | b"nclc"));
            let keep = ENTRY_CHILDREN.iter().any(|k| **k == kind) && (&kind != b"colr" || colour_numbers);
            if !keep {
                blank(data, child, header, child_end);
            }
            child = child_end;
        }
        // Bytes after the last child that do not form a box are zeroed too.
        if child < end {
            data[child..end].fill(0);
        }
        at = end;
    }
    data[at.min(to)..to].fill(0); // bytes too few to be an entry
    Some(())
}

/// `dref`: only self-contained entries (a `url ` box with flag 1 and no location); a clip that
/// points at another file (a path that can name a person or a game) is refused.
fn self_contained(data: &mut [u8], from: usize, to: usize) -> Option<()> {
    let mut at = from.checked_add(8)?; // version, flags, entry count
    while at + 8 <= to {
        let (kind, header, end) = box_at(data, at, to)?;
        if &kind != b"url " || end - at != header + 4 || data[at + header + 1..at + header + 4] != [0, 0, 1] {
            return None;
        }
        at = end;
    }
    data[at.min(to)..to].fill(0); // bytes too few to be an entry
    Some(())
}
