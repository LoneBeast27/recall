//! The repair of a long recording cut off mid-write (part of `longrec`, run by `recover`).
//!
//! A long recording is a fragmented MP4 written in whole fragments, so a cut-off `.part` plays
//! to its last whole fragment. What a kill or a power cut leaves wrong: a torn last fragment, a
//! zero-filled tail (a power cut after Windows extended the file), and a movie length of 0
//! (`empty_moov`: the length is patched only at Stop), so players and the library cannot tell
//! how long it is. The repair, in place, with an undo record so the original can always come
//! back:
//! 1. Scan the top-level boxes (headers only): the movie header, every whole `moof`+`mdat`
//!    pair, and a trailing run of at least [`ZERO_RUN`] zero bytes.
//! 2. Save what will be cut (`<part>.tail`, flushed) and the undo record (the in-progress list:
//!    the length, the cut, the `mvhd` duration's place and old bytes, the tail's hash).
//! 3. Cut at the last whole fragment before the zeros; set the `mvhd` duration to the end of the
//!    last video packet (what Stop writes).
//! 4. Verify: it demuxes, no packet is all zeros, the length read back matches the video.
//!    On a failure: [`undo`] (the original bytes, exactly) and recovered as it is.
//! 5. Done: the undo record is cleared and the tail goes to the Recycle Bin.
//!
//! A repair cut off midway (the engine killed during it) is undone at the next start before it
//! runs again. [`Hooks`] stop it at a step (tests only).
use std::fs::OpenOptions;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use ffmpeg_next as ff;

use crate::encoder::in_progress::{self, Entry, Undo};

/// A trailing run of at least this many zero bytes is a power cut's, never media.
pub(crate) const ZERO_RUN: u64 = 256;

/// Stop the repair at a step (tests: a kill midway, a failed verify).
#[derive(Debug, Default, Clone, Copy)]
pub(crate) struct Hooks {
    pub stop_after_truncate: bool,
    pub stop_after_patch: bool,
    pub fail_verify: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Repair {
    /// Cut to its last whole fragment and given its length (seconds).
    Repaired { seconds: f64 },
    /// Already whole with its length set.
    Whole { seconds: Option<f64> },
    /// Left exactly as it was (why): it plays as far as it plays, its length unknown.
    AsIs(String),
    /// No whole fragment: nothing in it plays (why).
    Unrecoverable(String),
    /// An earlier repair that cannot be undone (why): left as it is, its record and tail kept.
    Stuck(String),
    /// A [`Hooks`] stop: the undo record is in place.
    Interrupted,
}

/// `<part>.tail`: the bytes a repair cut off, kept until it verified.
pub(crate) fn tail_path(part: &Path) -> PathBuf {
    let mut s = part.as_os_str().to_owned();
    s.push(".tail");
    PathBuf::from(s)
}

pub(crate) fn repair(part: &Path) -> Repair {
    repair_with(part, Hooks::default())
}

/// What the top-level boxes say.
struct Scan {
    len: u64,
    /// The `mvhd` duration field: where, its width (4 or 8 bytes), its value, the timescale.
    mvhd_at: u64,
    wide: bool,
    duration: u64,
    timescale: u32,
    /// Where each whole `moof`+`mdat` pair ends.
    fragments: Vec<u64>,
    /// Where a trailing zero run of at least [`ZERO_RUN`] bytes starts (else the length).
    zeros_at: u64,
}

fn read_at(file: &mut std::fs::File, at: u64, buf: &mut [u8]) -> std::io::Result<()> {
    file.seek(SeekFrom::Start(at))?;
    file.read_exact(buf)
}

fn scan(part: &Path) -> Result<Scan, String> {
    let mut file = std::fs::File::open(part).map_err(|e| format!("open: {e}"))?;
    let len = file.metadata().map_err(|e| e.to_string())?.len();
    let (mut pos, mut after_moof, mut fragments, mut mvhd) = (0u64, false, Vec::new(), None);
    while pos + 8 <= len {
        let mut head = [0u8; 16];
        read_at(&mut file, pos, &mut head[..8]).map_err(|e| e.to_string())?;
        let (mut size, mut header) = (u32::from_be_bytes(head[..4].try_into().unwrap_or_default()) as u64, 8u64);
        if size == 1 && pos + 16 <= len {
            read_at(&mut file, pos + 8, &mut head[8..16]).map_err(|e| e.to_string())?;
            (size, header) = (u64::from_be_bytes(head[8..16].try_into().unwrap_or_default()), 16);
        }
        if size < header || pos + size > len {
            break;
        }
        match &head[4..8] {
            b"moov" => mvhd = find_mvhd(&mut file, pos + header, pos + size),
            b"moof" => after_moof = true,
            b"mdat" if after_moof => {
                fragments.push(pos + size);
                after_moof = false;
            }
            _ => {}
        }
        pos += size;
    }
    let (mvhd_at, wide, duration, timescale) = mvhd.ok_or("no movie header")?;
    Ok(Scan { len, mvhd_at, wide, duration, timescale, fragments, zeros_at: len - trailing_zeros(&mut file, len) })
}

/// (duration field offset, 8-byte field, duration, timescale) of the `mvhd` in `start..end`.
fn find_mvhd(file: &mut std::fs::File, start: u64, end: u64) -> Option<(u64, bool, u64, u32)> {
    let mut pos = start;
    while pos + 8 <= end {
        let mut head = [0u8; 8];
        read_at(file, pos, &mut head).ok()?;
        let size = u32::from_be_bytes(head[..4].try_into().ok()?) as u64;
        if size < 8 || pos + size > end {
            return None;
        }
        if &head[4..] == b"mvhd" {
            let mut version = [0u8; 1];
            read_at(file, pos + 8, &mut version).ok()?;
            let wide = version[0] == 1;
            let ts_at = pos + 12 + if wide { 16 } else { 8 };
            let mut ts = [0u8; 4];
            read_at(file, ts_at, &mut ts).ok()?;
            let mut d = [0u8; 8];
            let n = if wide { 8 } else { 4 };
            read_at(file, ts_at + 4, &mut d[8 - n..]).ok()?;
            return Some((ts_at + 4, wide, u64::from_be_bytes(d), u32::from_be_bytes(ts)));
        }
        pos += size;
    }
    None
}

/// Zero bytes at the end of the file, counted when at least [`ZERO_RUN`].
fn trailing_zeros(file: &mut std::fs::File, len: u64) -> u64 {
    let (mut zeros, mut buf) = (0u64, vec![0u8; 64 * 1024]);
    while zeros < len {
        let n = buf.len().min((len - zeros) as usize);
        if read_at(file, len - zeros - n as u64, &mut buf[..n]).is_err() {
            break;
        }
        match buf[..n].iter().rposition(|b| *b != 0) {
            Some(last) => {
                zeros += (n - last - 1) as u64;
                break;
            }
            None => zeros += n as u64,
        }
    }
    if zeros >= ZERO_RUN { zeros } else { 0 }
}

/// (packets, all-zero packets, end of the last video packet in µs) as FFmpeg reads the file.
fn demux(path: &Path) -> Result<(usize, usize, u64), String> {
    let _ = ff::init();
    let mut input = ff::format::input(&path).map_err(|e| format!("demux: {e}"))?;
    let video = input.streams().best(ff::media::Type::Video).map(|s| (s.index(), s.time_base())).ok_or("no video stream")?;
    let (mut all, mut zero, mut end) = (0, 0, 0f64);
    for (stream, p) in input.packets() {
        all += 1;
        if p.data().is_some_and(|d| d.iter().all(|b| *b == 0)) {
            zero += 1;
        }
        if stream.index() == video.0 {
            end = end.max((p.pts().unwrap_or(0) + p.duration()) as f64 * f64::from(video.1));
        }
    }
    Ok((all, zero, (end * 1e6).round() as u64))
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn unhex(text: &str) -> Vec<u8> {
    (0..text.len() / 2).filter_map(|i| u8::from_str_radix(&text[i * 2..i * 2 + 2], 16).ok()).collect()
}

fn write_at(part: &Path, at: u64, bytes: &[u8]) -> std::io::Result<()> {
    let mut file = OpenOptions::new().write(true).open(part)?;
    file.seek(SeekFrom::Start(at))?;
    file.write_all(bytes)?;
    file.sync_all()
}

fn field(wide: bool, value: u64) -> Vec<u8> {
    if wide { value.to_be_bytes().to_vec() } else { (value.min(u32::MAX as u64) as u32).to_be_bytes().to_vec() }
}

/// [`repair`] with [`Hooks`].
pub(crate) fn repair_with(part: &Path, hooks: Hooks) -> Repair {
    // The saved tail is gone for good (nothing can put the original back) and the file plays as
    // it is now: it ends as a recovered recording at its current length, never a `.part` forever.
    if let Some(u) = in_progress::get(part).and_then(|e| e.undo) {
        if !Path::new(&u.tail).exists() && crate::encoder::part_recover::plays(part) {
            let _ = in_progress::update(part, |e| e.undo = None);
            return Repair::AsIs(format!("its saved tail {} is gone for good; recovered at its current length", u.tail));
        }
    }
    if in_progress::get(part).is_some_and(|e| e.undo.is_some()) {
        if let Err(e) = undo(part) {
            return Repair::Stuck(format!("an earlier repair could not be undone: {e}"));
        }
    }
    let s = match scan(part) {
        Ok(s) => s,
        Err(e) => return Repair::AsIs(e),
    };
    let Some(keep) = s.fragments.iter().rev().copied().find(|end| *end <= s.zeros_at) else {
        return Repair::Unrecoverable("no whole fragment".into());
    };
    let secs = |units: u64| (s.timescale > 0).then(|| units as f64 / f64::from(s.timescale));
    if keep == s.len && s.duration > 0 {
        return Repair::Whole { seconds: secs(s.duration) };
    }
    match apply(part, &s, keep, hooks) {
        Ok(Some(seconds)) => Repair::Repaired { seconds },
        Ok(None) => Repair::Interrupted,
        Err(why) => match undo(part) {
            Ok(_) => Repair::AsIs(why),
            Err(e) => Repair::Stuck(format!("{why}; the undo failed too: {e}")),
        },
    }
}

/// Steps 2-5 (see the module doc). `Ok(None)`: stopped by a hook. `Err`: undo it.
fn apply(part: &Path, s: &Scan, keep: u64, hooks: Hooks) -> Result<Option<f64>, String> {
    let mut file = std::fs::File::open(part).map_err(|e| e.to_string())?;
    let mut old = vec![0u8; if s.wide { 8 } else { 4 }];
    read_at(&mut file, s.mvhd_at, &mut old).map_err(|e| format!("read mvhd: {e}"))?;
    let tail_at = tail_path(part);
    // The tail goes through a bounded buffer (it can be up to a whole torn fragment, or a
    // power cut's long zero run), hashed as it goes.
    file.seek(SeekFrom::Start(keep)).map_err(|e| format!("read the tail: {e}"))?;
    let hash = copy_hashed(&mut (&mut file).take(s.len - keep), &tail_at).map_err(|e| format!("save the tail: {e}"))?;
    let record = Undo { len: s.len, keep, mvhd_at: s.mvhd_at, mvhd_old: hex(&old), tail: tail_at.to_string_lossy().into_owned(), tail_hash: hash };
    if in_progress::get(part).is_none() {
        in_progress::add(Entry::long(part))?;
    }
    if !in_progress::update(part, |e| e.undo = Some(record))? {
        return Err("no list entry to hold the undo record".into());
    }
    OpenOptions::new().write(true).open(part).and_then(|f| f.set_len(keep).and_then(|()| f.sync_all())).map_err(|e| format!("cut: {e}"))?;
    if hooks.stop_after_truncate {
        return Ok(None);
    }
    let (_, _, end_us) = demux(part)?;
    let units = (end_us as u128 * u128::from(s.timescale) / 1_000_000) as u64;
    write_at(part, s.mvhd_at, &field(s.wide, units)).map_err(|e| format!("patch mvhd: {e}"))?;
    if hooks.stop_after_patch {
        return Ok(None);
    }
    let (packets, zero, end_after) = demux(part)?;
    let read_back = scan(part).ok().filter(|t| t.timescale > 0).map(|t| t.duration as f64 / f64::from(t.timescale));
    let seconds = end_after as f64 / 1e6;
    let length_ok = read_back.is_some_and(|r| (r - seconds).abs() < 0.05);
    if hooks.fail_verify || packets == 0 || zero > 0 || !length_ok {
        return Err(format!("verify failed: {packets} packets, {zero} all zero, length {read_back:?} s for {seconds:.3} s of video"));
    }
    in_progress::update(part, |e| e.undo = None)?;
    let _ = crate::encoder::recycle::recycle(&tail_at);
    Ok(Some(seconds))
}

/// Copy `from` into a new file `to` through a 1 MiB buffer, flushed to disk; its FNV hash (hex).
fn copy_hashed(from: &mut impl Read, to: &Path) -> std::io::Result<String> {
    let mut out = std::fs::File::create(to)?;
    let (mut buf, mut hash) = (vec![0u8; 1 << 20], in_progress::FNV_START);
    loop {
        let n = from.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hash = in_progress::fnv_update(hash, &buf[..n]);
        out.write_all(&buf[..n])?;
    }
    out.sync_all()?;
    Ok(format!("{hash:016x}"))
}

/// The FNV hash (hex) and length of the file at `path`, read through a bounded buffer.
fn hash_file(path: &Path) -> std::io::Result<(String, u64)> {
    let mut file = std::fs::File::open(path)?;
    let (mut buf, mut hash, mut len) = (vec![0u8; 1 << 20], in_progress::FNV_START, 0u64);
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            return Ok((format!("{hash:016x}"), len));
        }
        hash = in_progress::fnv_update(hash, &buf[..n]);
        len += n as u64;
    }
}

/// Put back the original bytes of a repair that did not finish (its undo record in the list):
/// the cut-off tail appended, the old `mvhd` duration written. `Ok(false)`: nothing to undo.
pub(crate) fn undo(part: &Path) -> Result<bool, String> {
    let Some(u) = in_progress::get(part).and_then(|e| e.undo) else { return Ok(false) };
    let result = (|| -> Result<(), String> {
        let (hash, len) = hash_file(Path::new(&u.tail)).map_err(|e| format!("read the saved tail {}: {e}", u.tail))?;
        if len != u.len - u.keep || hash != u.tail_hash {
            return Err(format!("the saved tail {} is not the one cut off", u.tail));
        }
        let mut file = OpenOptions::new().write(true).open(part).map_err(|e| e.to_string())?;
        file.set_len(u.keep).map_err(|e| e.to_string())?;
        let mut tail = std::fs::File::open(&u.tail).map_err(|e| e.to_string())?;
        file.seek(SeekFrom::End(0)).and_then(|_| std::io::copy(&mut tail, &mut file)).map_err(|e| format!("append the tail: {e}"))?;
        file.seek(SeekFrom::Start(u.mvhd_at)).and_then(|_| file.write_all(&unhex(&u.mvhd_old))).map_err(|e| format!("restore mvhd: {e}"))?;
        file.sync_all().map_err(|e| e.to_string())?;
        let len = file.metadata().map(|m| m.len()).unwrap_or(0);
        (len == u.len).then_some(()).ok_or_else(|| format!("undone to {len} B, not {} B", u.len))
    })();
    // A failed undo keeps its record and its tail (the original can still be put back by hand).
    result?;
    in_progress::update(part, |e| e.undo = None)?;
    let _ = crate::encoder::recycle::recycle(Path::new(&u.tail));
    Ok(true)
}
