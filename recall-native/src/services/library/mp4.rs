//! MP4 duration from `moov/mvhd` (and the video track's own length from its `mdhd`, and where
//! its last frame starts from its sample tables) by seeking over boxes (mdat is never read).
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

/// Guards against a corrupt file whose boxes never advance.
const MAX_BOXES: usize = 4096;

pub(super) fn duration(path: &Path) -> Option<f64> {
    let mut file = std::fs::File::open(path).ok()?;
    duration_of(&mut file)
}

/// Seconds from `mvhd` duration/timescale; a fragmented file with an empty `mvhd`
/// falls back to `moov/mvex/mehd`.
pub(super) fn duration_of<R: Read + Seek>(reader: &mut R) -> Option<f64> {
    let end = reader.seek(SeekFrom::End(0)).ok()?;
    let moov = find_box(reader, 0, end, b"moov")?;
    let mvhd = find_box(reader, moov.0, moov.1, b"mvhd")?;
    let (timescale, mut units) = read_mvhd(reader, mvhd)?;
    if units == 0 {
        let mvex = find_box(reader, moov.0, moov.1, b"mvex")?;
        let mehd = find_box(reader, mvex.0, mvex.1, b"mehd")?;
        units = read_mehd(reader, mehd)?;
    }
    if timescale == 0 || units == 0 {
        return None;
    }
    let seconds = units as f64 / f64::from(timescale);
    seconds.is_finite().then_some(seconds)
}

/// Seconds of the first video track (a `trak` whose `mdia/hdlr` is `vide`) from its `mdhd`.
/// The picture ends here even when the sound, and so `mvhd`, runs a little longer.
pub(super) fn video_duration_of<R: Read + Seek>(reader: &mut R) -> Option<f64> {
    let (_, mdia) = video_track(reader)?;
    // mdhd starts like mvhd: version/flags, two times, timescale, duration.
    let mdhd = find_box(reader, mdia.0, mdia.1, b"mdhd")?;
    let (timescale, units) = read_mvhd(reader, mdhd)?;
    let seconds = (timescale > 0 && units > 0).then(|| units as f64 / f64::from(timescale))?;
    seconds.is_finite().then_some(seconds)
}

/// Seconds at which the first video track's last frame (in display order) starts. The track's
/// `mdhd` length says nothing about this: the last sample can be held for a long time.
pub(super) fn last_frame_of<R: Read + Seek>(reader: &mut R) -> Option<f64> {
    let (trak, mdia) = video_track(reader)?;
    let mdhd = find_box(reader, mdia.0, mdia.1, b"mdhd")?;
    let (timescale, _) = read_mvhd(reader, mdhd)?;
    let minf = find_box(reader, mdia.0, mdia.1, b"minf")?;
    let stbl = find_box(reader, minf.0, minf.1, b"stbl")?;
    let stts = find_box(reader, stbl.0, stbl.1, b"stts")?;
    let durations = read_runs(reader, stts)?;
    let offsets = find_box(reader, stbl.0, stbl.1, b"ctts").and_then(|ctts| read_runs(reader, ctts)).unwrap_or_default();
    let last = last_presented(&durations, &offsets)? - edit_start(reader, trak).unwrap_or(0);
    (timescale > 0).then(|| last.max(0) as f64 / f64::from(timescale))
}

/// (`trak`, `mdia`) of the first track whose `hdlr` is `vide`.
pub(super) fn video_track<R: Read + Seek>(reader: &mut R) -> Option<((u64, u64), (u64, u64))> {
    let end = reader.seek(SeekFrom::End(0)).ok()?;
    let moov = find_box(reader, 0, end, b"moov")?;
    let mut at = moov.0;
    for _ in 0..MAX_BOXES {
        let trak = find_box(reader, at, moov.1, b"trak")?;
        at = trak.1;
        let Some(mdia) = find_box(reader, trak.0, trak.1, b"mdia") else { continue };
        let Some(hdlr) = find_box(reader, mdia.0, mdia.1, b"hdlr") else { continue };
        // hdlr payload: version/flags, pre_defined, then the handler type.
        let mut handler = [0u8; 4];
        if hdlr.1.saturating_sub(hdlr.0) < 12 || reader.seek(SeekFrom::Start(hdlr.0 + 8)).is_err() || reader.read_exact(&mut handler).is_err() {
            continue;
        }
        if &handler == b"vide" {
            return Some((trak, mdia));
        }
    }
    None
}

/// The (sample count, value) runs of an `stts` or `ctts` box.
pub(super) fn read_runs<R: Read + Seek>(reader: &mut R, (start, end): (u64, u64)) -> Option<Vec<(u32, u32)>> {
    reader.seek(SeekFrom::Start(start + 4)).ok()?;
    let count = u64::from(read_u32(reader)?).min(end.saturating_sub(start + 8) / 8);
    let mut bytes = vec![0u8; count as usize * 8];
    reader.read_exact(&mut bytes).ok()?;
    let word = |at: &[u8]| u32::from_be_bytes([at[0], at[1], at[2], at[3]]);
    Some(bytes.chunks_exact(8).map(|run| (word(run), word(&run[4..]))).collect())
}

/// The latest presentation time among the samples, in track units: decode times from the
/// `stts` runs plus the `ctts` offsets. None without samples.
pub(super) fn last_presented(durations: &[(u32, u32)], offsets: &[(u32, u32)]) -> Option<i64> {
    let mut offsets = offsets.iter().copied();
    let (mut left, mut offset) = (0u32, 0u32);
    let (mut decode, mut latest) = (0i64, None);
    for &(count, delta) in durations {
        let mut remaining = count;
        while remaining > 0 {
            if left == 0 {
                (left, offset) = offsets.next().unwrap_or((u32::MAX, 0));
            }
            let run = remaining.min(left);
            if run > 0 {
                // Within a run the offset is fixed, so its last sample presents last.
                let shown = decode + i64::from(run - 1) * i64::from(delta) + i64::from(offset as i32);
                latest = latest.max(Some(shown));
            }
            decode += i64::from(run) * i64::from(delta);
            remaining -= run;
            left -= run;
        }
    }
    latest
}

/// The media time the first real `edts/elst` entry starts at (B-frame clips start past 0).
pub(super) fn edit_start<R: Read + Seek>(reader: &mut R, trak: (u64, u64)) -> Option<i64> {
    let edts = find_box(reader, trak.0, trak.1, b"edts")?;
    let (start, end) = find_box(reader, edts.0, edts.1, b"elst")?;
    reader.seek(SeekFrom::Start(start)).ok()?;
    let wide = read_u32(reader)? >> 24 == 1;
    let count = read_u32(reader)?;
    for _ in 0..count.min(16) {
        if reader.stream_position().ok()? >= end {
            return None;
        }
        // Entry: segment duration, media time (-1 means empty), rate.
        let media_time = if wide {
            let mut entry = [0u8; 20];
            reader.read_exact(&mut entry).ok()?;
            i64::from_be_bytes(entry[8..16].try_into().ok()?)
        } else {
            let mut entry = [0u8; 12];
            reader.read_exact(&mut entry).ok()?;
            i64::from(i32::from_be_bytes(entry[4..8].try_into().ok()?))
        };
        if media_time >= 0 {
            return Some(media_time);
        }
    }
    None
}

pub(super) fn read_u32<R: Read>(reader: &mut R) -> Option<u32> {
    let mut value = [0u8; 4];
    reader.read_exact(&mut value).ok()?;
    Some(u32::from_be_bytes(value))
}

/// (payload start, box end) of the first `kind` child in `[start, end)`.
pub(super) fn find_box<R: Read + Seek>(reader: &mut R, start: u64, end: u64, kind: &[u8; 4]) -> Option<(u64, u64)> {
    let mut pos = start;
    for _ in 0..MAX_BOXES {
        if pos.checked_add(8)? > end {
            return None;
        }
        reader.seek(SeekFrom::Start(pos)).ok()?;
        let mut header = [0u8; 8];
        reader.read_exact(&mut header).ok()?;
        let size32 = u32::from_be_bytes([header[0], header[1], header[2], header[3]]);
        let (header_len, size) = match size32 {
            0 => (8, end - pos),
            1 => {
                let mut large = [0u8; 8];
                reader.read_exact(&mut large).ok()?;
                (16, u64::from_be_bytes(large))
            }
            n => (8, u64::from(n)),
        };
        if size < header_len {
            return None;
        }
        let box_end = pos.checked_add(size)?;
        if &header[4..8] == kind {
            return Some((pos + header_len, box_end.min(end)));
        }
        if box_end > end {
            return None;
        }
        pos = box_end;
    }
    None
}

/// (timescale, duration units). Version 0: 32-bit times; version 1: 64-bit.
/// All-ones duration means "unknown" and reads as 0.
pub(super) fn read_mvhd<R: Read + Seek>(reader: &mut R, (start, end): (u64, u64)) -> Option<(u32, u64)> {
    let mut version = [0u8; 4];
    reader.seek(SeekFrom::Start(start)).ok()?;
    reader.read_exact(&mut version).ok()?;
    let (skip, wide) = match version[0] {
        0 => (8u64, false),
        1 => (16u64, true),
        _ => return None,
    };
    let needed = 4 + skip + 4 + if wide { 8 } else { 4 };
    if end.saturating_sub(start) < needed {
        return None;
    }
    reader.seek(SeekFrom::Start(start + 4 + skip)).ok()?;
    let mut timescale = [0u8; 4];
    reader.read_exact(&mut timescale).ok()?;
    let units = read_units(reader, wide)?;
    Some((u32::from_be_bytes(timescale), units))
}

/// `mehd` fragment_duration: version/flags, then a 32-bit (v0) or 64-bit (v1) value.
fn read_mehd<R: Read + Seek>(reader: &mut R, (start, end): (u64, u64)) -> Option<u64> {
    let mut version = [0u8; 4];
    reader.seek(SeekFrom::Start(start)).ok()?;
    reader.read_exact(&mut version).ok()?;
    let wide = version[0] == 1;
    if end.saturating_sub(start) < 4 + if wide { 8 } else { 4 } {
        return None;
    }
    read_units(reader, wide)
}

fn read_units<R: Read>(reader: &mut R, wide: bool) -> Option<u64> {
    if wide {
        let mut value = [0u8; 8];
        reader.read_exact(&mut value).ok()?;
        let value = u64::from_be_bytes(value);
        Some(if value == u64::MAX { 0 } else { value })
    } else {
        let mut value = [0u8; 4];
        reader.read_exact(&mut value).ok()?;
        let value = u32::from_be_bytes(value);
        Some(if value == u32::MAX { 0 } else { u64::from(value) })
    }
}
