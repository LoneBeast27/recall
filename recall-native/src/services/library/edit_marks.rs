//! Is a file in an imported folder a recording (raw or only trimmed), or an export from a video
//! editor? Editors leave their name in the file's metadata (Premiere Pro's XMP `CreatorTool`,
//! DaVinci Resolve's encoder tag, ...); recorders and trimmers do not. Only metadata is read:
//! an MP4/MOV's `moov` and top-level `uuid`/`meta`/`udta` boxes (never `mdat`), an MKV's first
//! and last MiB. Read-only: the file is opened for reading and nothing else.
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

/// What one look at the file found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    /// A recording, raw or trimmed: list it.
    Clip,
    /// An editor's export: skip it. The editor's name.
    Edited(&'static str),
    /// No video track (an audio file saved as .mp4, a broken file): skip it.
    NoVideo,
    /// No `moov` yet (still recording, a copy in flight): list it, look again later.
    Unready,
    /// The file could not be opened or read this time (locked, a network blip): list it, look
    /// again later, and never take it for a broken file.
    Unreadable,
}

/// Lowercase needle → the editor's name. Names that a recorder from the same company also writes
/// (CyberLink, TechSmith, Wondershare, Blackmagic) are left out: a raw clip must never be skipped.
/// MainConcept is the H.264/AAC encoder inside Premiere Pro and Media Encoder; its handler names
/// ("Mainconcept Video Media Handler") stay even when the export drops its XMP. The XMP toolkit
/// tag marks any Adobe app. No recorder sampled on the maintainer's disk (SteelSeries, GeForce, MeldStudio,
/// Recall: 35 files, 2026-11-05) writes either.
const MARKERS: [(&str, &str); 21] = [
    ("adobe premiere", "Adobe Premiere Pro"),
    ("premiere pro", "Adobe Premiere Pro"),
    ("premiere rush", "Adobe Premiere Rush"),
    ("adobe media encoder", "Adobe Media Encoder"),
    ("after effects", "Adobe After Effects"),
    ("davinci resolve", "DaVinci Resolve"),
    ("vegas pro", "Vegas Pro"),
    ("sony vegas", "Vegas Pro"),
    ("magix", "MAGIX"),
    ("final cut", "Final Cut Pro"),
    ("capcut", "CapCut"),
    ("filmora", "Filmora"),
    ("clipchamp", "Clipchamp"),
    ("camtasia", "Camtasia"),
    ("powerdirector", "PowerDirector"),
    ("hitfilm", "HitFilm"),
    ("imovie", "iMovie"),
    ("kdenlive", "Kdenlive"),
    ("openshot", "OpenShot"),
    ("mainconcept", "Adobe Premiere Pro or Media Encoder"),
    ("adobe:ns:meta", "an Adobe app"),
];

/// Upper bound on metadata read per box (a long recording's `moov` is ~10 MiB).
const MAX_BOX_READ: u64 = 64 << 20;
/// MKV: bytes read from each end.
const MKV_WINDOW: u64 = 1 << 20;
const MAX_BOXES: usize = 4096;

pub fn inspect(path: &Path) -> Verdict {
    let Ok(mut file) = std::fs::File::open(path) else { return Verdict::Unreadable };
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or_default().to_ascii_lowercase();
    if ext == "mkv" || ext == "webm" { inspect_mkv(&mut file) } else { inspect_mp4(&mut file) }
}

pub(crate) fn inspect_mp4<R: Read + Seek>(reader: &mut R) -> Verdict {
    let Ok(end) = reader.seek(SeekFrom::End(0)) else { return Verdict::Unreadable };
    if end == 0 {
        // Stored as no video; a size change brings it back for another look.
        return Verdict::NoVideo;
    }
    let mut at = 0u64;
    let (mut moov, mut extra) = (None::<Vec<u8>>, Vec::<u8>::new());
    for _ in 0..MAX_BOXES {
        if at + 8 > end {
            break;
        }
        let Some((kind, body, next)) = box_at(reader, at, end) else { break };
        match &kind {
            b"moov" => match read_span(reader, body, next) {
                Some(bytes) => moov = Some(bytes),
                None => return Verdict::Unreadable,
            },
            b"uuid" | b"meta" | b"udta" | b"Xtra" => extra.extend(read_span(reader, body, next).unwrap_or_default()),
            _ => {}
        }
        if next <= at {
            break;
        }
        at = next;
    }
    let Some(mut moov) = moov else { return Verdict::Unready };
    if !has_handler(&moov, b"vide") {
        return Verdict::NoVideo;
    }
    // A timecode track: editors' QuickTime renders (After Effects) write one, recorders do not.
    let timecode = has_handler(&moov, b"tmcd");
    moov.extend(extra);
    marker(&mut moov).map(Verdict::Edited).unwrap_or(if timecode { Verdict::Edited("a video editor (timecode track)") } else { Verdict::Clip })
}

fn inspect_mkv<R: Read + Seek>(reader: &mut R) -> Verdict {
    let Ok(end) = reader.seek(SeekFrom::End(0)) else { return Verdict::Unreadable };
    let Some(mut bytes) = read_span(reader, 0, end.min(MKV_WINDOW)) else { return Verdict::Unreadable };
    if !bytes.starts_with(&[0x1A, 0x45, 0xDF, 0xA3]) {
        return Verdict::Unready;
    }
    if end > MKV_WINDOW {
        bytes.extend(read_span(reader, end.saturating_sub(MKV_WINDOW).max(MKV_WINDOW), end).unwrap_or_default());
    }
    marker(&mut bytes).map_or(Verdict::Clip, Verdict::Edited)
}

/// (type, body start, next box) of the box at `at`.
fn box_at<R: Read + Seek>(reader: &mut R, at: u64, end: u64) -> Option<([u8; 4], u64, u64)> {
    reader.seek(SeekFrom::Start(at)).ok()?;
    let mut head = [0u8; 8];
    reader.read_exact(&mut head).ok()?;
    let size = u64::from(u32::from_be_bytes([head[0], head[1], head[2], head[3]]));
    let kind = [head[4], head[5], head[6], head[7]];
    let (body, size) = match size {
        0 => (at + 8, end - at),
        1 => {
            let mut large = [0u8; 8];
            reader.read_exact(&mut large).ok()?;
            (at + 16, u64::from_be_bytes(large))
        }
        n => (at + 8, n),
    };
    (size >= body - at).then_some((kind, body, at.checked_add(size)?.min(end)))
}

fn read_span<R: Read + Seek>(reader: &mut R, start: u64, end: u64) -> Option<Vec<u8>> {
    let len = end.saturating_sub(start).min(MAX_BOX_READ);
    reader.seek(SeekFrom::Start(start)).ok()?;
    let mut bytes = Vec::with_capacity(len as usize);
    reader.take(len).read_to_end(&mut bytes).ok()?;
    Some(bytes)
}

/// Any `hdlr` box naming `kind`: version/flags (4), pre_defined (4), then the handler type.
fn has_handler(moov: &[u8], kind: &[u8; 4]) -> bool {
    moov.windows(4).enumerate().any(|(i, w)| w == b"hdlr" && moov.get(i + 12..i + 16) == Some(kind.as_slice()))
}

/// The first editor named in `bytes` (lowercased in place).
fn marker(bytes: &mut [u8]) -> Option<&'static str> {
    bytes.make_ascii_lowercase();
    MARKERS.iter().find(|(needle, _)| bytes.windows(needle.len()).any(|w| w == needle.as_bytes())).map(|(_, name)| *name)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn mp4_box(kind: &[u8; 4], body: &[u8]) -> Vec<u8> {
        let mut out = ((body.len() + 8) as u32).to_be_bytes().to_vec();
        out.extend_from_slice(kind);
        out.extend_from_slice(body);
        out
    }

    fn hdlr(handler: &[u8; 4], name: &str) -> Vec<u8> {
        let mut body = vec![0u8; 8];
        body.extend_from_slice(handler);
        body.extend_from_slice(&[0u8; 12]);
        body.extend_from_slice(name.as_bytes());
        body.push(0);
        mp4_box(b"hdlr", &body)
    }

    fn file(boxes: &[Vec<u8>]) -> Cursor<Vec<u8>> {
        Cursor::new(boxes.concat())
    }

    #[test]
    fn a_recorder_clip_is_a_clip() {
        let moov = mp4_box(b"moov", &mp4_box(b"trak", &hdlr(b"vide", "VideoHandler")));
        assert_eq!(inspect_mp4(&mut file(&[mp4_box(b"ftyp", b"isom"), mp4_box(b"mdat", &[7u8; 64]), moov])), Verdict::Clip);
    }

    #[test]
    fn premiere_xmp_after_mdat_marks_an_export() {
        let moov = mp4_box(b"moov", &mp4_box(b"trak", &hdlr(b"vide", "Mainconcept Video Media Handler")));
        let xmp = mp4_box(b"uuid", b"\xBE\x7A\xCF\xCB<xmp:CreatorTool>Adobe Premiere Pro 2024.0 (Windows)</xmp:CreatorTool>");
        assert_eq!(inspect_mp4(&mut file(&[mp4_box(b"ftyp", b"mp42"), moov, mp4_box(b"mdat", &[0u8; 32]), xmp])), Verdict::Edited("Adobe Premiere Pro"));
    }

    #[test]
    fn an_encoder_tag_inside_moov_marks_an_export() {
        let ilst = mp4_box(b"udta", &mp4_box(b"meta", b"\xA9tooDaVinci Resolve Studio"));
        let moov = mp4_box(b"moov", &[mp4_box(b"trak", &hdlr(b"vide", "")), ilst].concat());
        assert_eq!(inspect_mp4(&mut file(&[moov])), Verdict::Edited("DaVinci Resolve"));
    }

    #[test]
    fn adobe_exports_without_a_product_name_are_still_caught() {
        let mainconcept = mp4_box(b"moov", &mp4_box(b"trak", &hdlr(b"vide", "\x1fMainconcept Video Media Handler")));
        assert_eq!(inspect_mp4(&mut file(&[mainconcept])), Verdict::Edited("Adobe Premiere Pro or Media Encoder"));
        let moov = mp4_box(b"moov", &mp4_box(b"trak", &hdlr(b"vide", "")));
        let xmp = mp4_box(b"uuid", b"<x:xmpmeta xmlns:x=\"adobe:ns:meta/\" x:xmptk=\"Adobe XMP Core 7.1\">");
        assert_eq!(inspect_mp4(&mut file(&[moov, xmp])), Verdict::Edited("an Adobe app"));
        let render = mp4_box(b"moov", &[mp4_box(b"trak", &hdlr(b"vide", "")), mp4_box(b"trak", &hdlr(b"tmcd", ""))].concat());
        assert_eq!(inspect_mp4(&mut file(&[render])), Verdict::Edited("a video editor (timecode track)"));
    }

    #[test]
    fn audio_only_and_unfinished_files() {
        let sound = mp4_box(b"moov", &mp4_box(b"trak", &hdlr(b"soun", "SoundHandler")));
        assert_eq!(inspect_mp4(&mut file(&[sound])), Verdict::NoVideo);
        assert_eq!(inspect_mp4(&mut file(&[mp4_box(b"ftyp", b"isom"), mp4_box(b"mdat", &[1u8; 16])])), Verdict::Unready);
        assert_eq!(inspect_mp4(&mut Cursor::new(Vec::new())), Verdict::NoVideo, "an empty file");
    }

    #[test]
    fn a_box_that_runs_past_the_end_is_cut_not_followed() {
        let mut moov = mp4_box(b"moov", &mp4_box(b"trak", &hdlr(b"vide", "")));
        moov[..4].copy_from_slice(&u32::MAX.to_be_bytes());
        assert_eq!(inspect_mp4(&mut file(&[moov])), Verdict::Clip);
    }

    /// Read-only report over a real folder: `RECALL_INSPECT_DIR=<folder> cargo test -- --ignored
    /// report_real_folder --nocapture`. Opens each file for reading only.
    #[test]
    #[ignore]
    fn report_real_folder() {
        let Some(dir) = std::env::var_os("RECALL_INSPECT_DIR") else { return };
        let started = std::time::Instant::now();
        let clips = super::super::folders::scan_imported(Path::new(&dir), &[]);
        for clip in &clips {
            println!("{:?}\t{}", inspect(&clip.path), clip.path.display());
        }
        println!("{} files in {:.1} s", clips.len(), started.elapsed().as_secs_f64());
    }

    #[test]
    fn mkv_writing_app_is_read_from_either_end() {
        let mut head = vec![0x1A, 0x45, 0xDF, 0xA3];
        head.extend_from_slice(b"..WritingAppLavf60..");
        assert_eq!(inspect_mkv(&mut Cursor::new(head.clone())), Verdict::Clip);
        let mut cut = head.clone();
        cut.extend_from_slice(b"WritingApp: Kdenlive 24.08");
        assert_eq!(inspect_mkv(&mut Cursor::new(cut)), Verdict::Edited("Kdenlive"));
        assert_eq!(inspect_mkv(&mut Cursor::new(b"not a matroska file".to_vec())), Verdict::Unready);
    }
}
