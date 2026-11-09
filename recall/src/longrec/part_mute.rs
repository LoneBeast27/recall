//! Zero the device audio of a recovered long recording from a moment on (review F-4 (b), the
//! coordinator's decision). When an engine exits or is killed with a privacy-tightening line
//! (Alt+M, a removed source) it never applied, the controller passes the moment that line was
//! issued to the next engine (`RECALL_LONGREC_MUTE_FROM`, unix ms). That engine's `.part`
//! recovery rewrites, in place, every AAC sample of every device track from that moment on as
//! a silent frame of the same size (an AAC fill element pads it), so no offset in the file
//! moves: the video and the game's process-tree and app tracks are kept, the clip survives,
//! and the muted audio does not.
//!
//! Which audio tracks are device tracks, and when the part started, is in a sidecar next to
//! the part (`<part>.devices`), written when the part is created and removed when the part is
//! finished, kept or recovered.

use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

/// The controller's env var: unix ms of the first unapplied privacy line (absent: none).
pub const MUTE_FROM_ENV: &str = "RECALL_LONGREC_MUTE_FROM";
/// Zero this much before the moment too (the part's start time is wall-clock, ms).
const MARGIN_SECS: f64 = 0.5;

/// What the sidecar says.
#[derive(Debug, Clone, PartialEq)]
pub struct Sidecar {
    pub start_unix_ms: u64,
    /// Per audio track, in stream order: a device track (an input or an output endpoint).
    pub device: Vec<bool>,
}

pub fn sidecar_path(part: &Path) -> PathBuf {
    let mut s = part.as_os_str().to_owned();
    s.push(".devices");
    PathBuf::from(s)
}

/// Write the sidecar for a part created now with these tracks.
pub fn write_sidecar(part: &Path, device: &[bool]) -> std::io::Result<()> {
    let start = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0);
    let flags: Vec<&str> = device.iter().map(|d| if *d { "1" } else { "0" }).collect();
    std::fs::write(sidecar_path(part), format!("{start}\n{}\n", flags.join(",")))
}

pub fn read_sidecar(part: &Path) -> Option<Sidecar> {
    let text = std::fs::read_to_string(sidecar_path(part)).ok()?;
    let mut lines = text.lines();
    let start_unix_ms = lines.next()?.trim().parse().ok()?;
    let device = lines.next().unwrap_or("").split(',').filter(|f| !f.is_empty()).map(|f| f.trim() == "1").collect();
    Some(Sidecar { start_unix_ms, device })
}

/// Remove the sidecar (the part was finished, kept or recovered). Its own file, by exact path.
pub fn remove_sidecar(part: &Path) {
    let _ = std::fs::remove_file(sidecar_path(part));
}

/// The part timeline second from which `sidecar`'s device tracks are zeroed for a privacy
/// line issued at `mute_from_ms` (never below 0).
pub fn cutoff_secs(sidecar: &Sidecar, mute_from_ms: u64) -> f64 {
    ((mute_from_ms as f64 - sidecar.start_unix_ms as f64) / 1000.0 - MARGIN_SECS).max(0.0)
}

/// One audio track of the file.
#[derive(Debug, Clone)]
struct Track {
    id: u32,
    timescale: u32,
    channels: u16,
    default_duration: u32,
    default_size: u32,
}

/// One box header: (start, payload start, end, kind). None at the end or on a cut box.
fn box_at(file: &mut File, at: u64, end: u64) -> std::io::Result<Option<(u64, u64, u64, [u8; 4])>> {
    if at + 8 > end {
        return Ok(None);
    }
    file.seek(SeekFrom::Start(at))?;
    let mut h = [0u8; 16];
    file.read_exact(&mut h[..8])?;
    let small = u32::from_be_bytes(h[0..4].try_into().unwrap()) as u64;
    let kind: [u8; 4] = h[4..8].try_into().unwrap();
    let (size, header) = match small {
        1 => {
            file.read_exact(&mut h[8..16])?;
            (u64::from_be_bytes(h[8..16].try_into().unwrap()), 16)
        }
        0 => (end - at, 8),
        n => (n, 8),
    };
    if size < header || at + size > end {
        return Ok(None); // a box cut by the crash: the rest of the file is not trusted
    }
    Ok(Some((at, at + header, at + size, kind)))
}

/// The children of a box payload held in memory: (payload start, end, kind).
fn children(data: &[u8], mut at: usize, end: usize) -> Vec<(usize, usize, [u8; 4])> {
    let mut out = Vec::new();
    while at + 8 <= end {
        let size = u32::from_be_bytes(data[at..at + 4].try_into().unwrap()) as usize;
        let kind: [u8; 4] = data[at + 4..at + 8].try_into().unwrap();
        let (size, header) = if size == 1 && at + 16 <= end { (u64::from_be_bytes(data[at + 8..at + 16].try_into().unwrap()) as usize, 16) } else { (size, 8) };
        if size < header || at + size > end {
            break;
        }
        out.push((at + header, at + size, kind));
        at += size;
    }
    out
}

fn child<'a>(list: &'a [(usize, usize, [u8; 4])], kind: &[u8; 4]) -> Option<&'a (usize, usize, [u8; 4])> {
    list.iter().find(|c| &c.2 == kind)
}

fn be32(d: &[u8], at: usize) -> u32 {
    d.get(at..at + 4).map_or(0, |b| u32::from_be_bytes(b.try_into().unwrap()))
}

fn be64(d: &[u8], at: usize) -> u64 {
    d.get(at..at + 8).map_or(0, |b| u64::from_be_bytes(b.try_into().unwrap()))
}

/// Every audio track of `moov`, in trak order, with the `trex` defaults.
fn audio_tracks(moov: &[u8]) -> Vec<Track> {
    let top = children(moov, 0, moov.len());
    let trex: Vec<(u32, u32, u32)> = child(&top, b"mvex")
        .map(|m| children(moov, m.0, m.1).into_iter().filter(|c| &c.2 == b"trex").map(|t| (be32(moov, t.0 + 4), be32(moov, t.0 + 12), be32(moov, t.0 + 16))).collect())
        .unwrap_or_default();
    let mut out = Vec::new();
    for trak in top.iter().filter(|c| &c.2 == b"trak") {
        let tk = children(moov, trak.0, trak.1);
        let (Some(tkhd), Some(mdia)) = (child(&tk, b"tkhd"), child(&tk, b"mdia")) else { continue };
        let id = be32(moov, tkhd.0 + if moov[tkhd.0] == 1 { 20 } else { 12 });
        let md = children(moov, mdia.0, mdia.1);
        let (Some(mdhd), Some(hdlr), Some(minf)) = (child(&md, b"mdhd"), child(&md, b"hdlr"), child(&md, b"minf")) else { continue };
        if moov.get(hdlr.0 + 8..hdlr.0 + 12) != Some(b"soun") {
            continue;
        }
        let timescale = be32(moov, mdhd.0 + if moov[mdhd.0] == 1 { 20 } else { 12 });
        let channels = child(&children(moov, minf.0, minf.1), b"stbl")
            .and_then(|stbl| child(&children(moov, stbl.0, stbl.1), b"stsd").copied())
            .map(|stsd| u16::from_be_bytes([moov.get(stsd.0 + 8 + 8 + 16).copied().unwrap_or(0), moov.get(stsd.0 + 8 + 8 + 17).copied().unwrap_or(2)]))
            .unwrap_or(2);
        let (d, s) = trex.iter().find(|t| t.0 == id).map_or((0, 0), |t| (t.1, t.2));
        out.push(Track { id, timescale, channels, default_duration: d, default_size: s });
    }
    out
}

/// A sample to rewrite: (file offset, size, track index among the audio tracks).
type Hit = (u64, u32, usize);

/// The samples of `moof` (at `moof_at` in the file) that belong to a zeroed track and end
/// after `from_secs`.
fn hits_in_moof(moof: &[u8], moof_at: u64, tracks: &[Track], zero: &[bool], from_secs: f64) -> Vec<Hit> {
    let mut out = Vec::new();
    for traf in children(moof, 0, moof.len()).into_iter().filter(|c| &c.2 == b"traf") {
        let tr = children(moof, traf.0, traf.1);
        let Some(tfhd) = child(&tr, b"tfhd") else { continue };
        let flags = be32(moof, tfhd.0) & 0x00ff_ffff;
        let id = be32(moof, tfhd.0 + 4);
        let Some(ti) = tracks.iter().position(|t| t.id == id) else { continue };
        if !zero.get(ti).copied().unwrap_or(false) {
            continue;
        }
        let mut at = tfhd.0 + 8;
        let mut base = moof_at;
        if flags & 0x1 != 0 {
            base = be64(moof, at);
            at += 8;
        }
        if flags & 0x2 != 0 {
            at += 4;
        }
        let mut dflt_dur = tracks[ti].default_duration;
        let mut dflt_size = tracks[ti].default_size;
        if flags & 0x8 != 0 {
            dflt_dur = be32(moof, at);
            at += 4;
        }
        if flags & 0x10 != 0 {
            dflt_size = be32(moof, at);
        }
        let mut time = child(&tr, b"tfdt").map_or(0, |t| if moof[t.0] == 1 { be64(moof, t.0 + 4) } else { be32(moof, t.0 + 4) as u64 });
        let mut data = base;
        for trun in tr.iter().filter(|c| &c.2 == b"trun") {
            let f = be32(moof, trun.0) & 0x00ff_ffff;
            let count = be32(moof, trun.0 + 4);
            let mut p = trun.0 + 8;
            if f & 0x1 != 0 {
                data = base.wrapping_add_signed(be32(moof, p) as i32 as i64);
                p += 4;
            }
            if f & 0x4 != 0 {
                p += 4;
            }
            for _ in 0..count {
                let dur = if f & 0x100 != 0 { let v = be32(moof, p); p += 4; v } else { dflt_dur };
                let size = if f & 0x200 != 0 { let v = be32(moof, p); p += 4; v } else { dflt_size };
                p += if f & 0x400 != 0 { 4 } else { 0 } + if f & 0x800 != 0 { 4 } else { 0 };
                let end_secs = (time + dur as u64) as f64 / tracks[ti].timescale.max(1) as f64;
                if end_secs > from_secs && size > 0 {
                    out.push((data, size, ti));
                }
                time += dur as u64;
                data += size as u64;
            }
        }
    }
    out
}

/// Zero every sample of the audio tracks flagged in `device` (stream order) that ends after
/// `from_secs`, in place. `silent(rate, channels)` gives one silent AAC frame. The number of
/// samples rewritten.
pub fn zero_device_audio(path: &Path, device: &[bool], from_secs: f64, silent: &dyn Fn(u32, u16) -> Option<Vec<u8>>) -> Result<usize, String> {
    let mut file = OpenOptions::new().read(true).write(true).open(path).map_err(|e| e.to_string())?;
    let end = file.metadata().map_err(|e| e.to_string())?.len();
    let mut at = 0;
    let mut tracks: Vec<Track> = Vec::new();
    let mut hits: Vec<Hit> = Vec::new();
    while let Some((start, payload, stop, kind)) = box_at(&mut file, at, end).map_err(|e| e.to_string())? {
        if &kind == b"moov" || &kind == b"moof" {
            let mut data = vec![0u8; (stop - payload) as usize];
            file.seek(SeekFrom::Start(payload)).map_err(|e| e.to_string())?;
            file.read_exact(&mut data).map_err(|e| e.to_string())?;
            if &kind == b"moov" {
                tracks = audio_tracks(&data);
            } else {
                hits.extend(hits_in_moof(&data, start, &tracks, device, from_secs));
            }
        }
        at = stop;
    }
    if tracks.is_empty() {
        return Err("no audio tracks found".into());
    }
    let mut frames = std::collections::HashMap::new();
    let mut done = 0;
    for (offset, size, ti) in hits {
        if offset + size as u64 > end {
            continue; // in a cut mdat: no player reads it
        }
        let t = &tracks[ti];
        let frame = frames.entry(ti).or_insert_with(|| silent(t.timescale, t.channels)).clone().ok_or("no silent AAC frame")?;
        // A frame smaller than a silent one carries next to nothing; it is zeroed byte for byte.
        let bytes = super::aac_fill::pad_to(&frame, size as usize).unwrap_or_else(|| vec![0u8; size as usize]);
        file.seek(SeekFrom::Start(offset)).map_err(|e| e.to_string())?;
        file.write_all(&bytes).map_err(|e| e.to_string())?;
        done += 1;
    }
    file.sync_all().map_err(|e| e.to_string())?;
    Ok(done)
}

/// One silent AAC frame for `rate` and `channels`, made by the same encoder settings as the
/// recording's tracks (`fmp4.rs` `AacLane`): a steady-state frame of encoded zeros.
pub fn silent_frame(rate: u32, channels: u16) -> Option<Vec<u8>> {
    use ffmpeg_next as ff;
    let layout = if channels <= 1 { ff::ChannelLayout::MONO } else { ff::ChannelLayout::STEREO };
    let format = ff::format::Sample::F32(ff::format::sample::Type::Planar);
    let codec = ff::encoder::find(ff::codec::Id::AAC)?;
    let mut ctx = ff::codec::context::Context::new_with_codec(codec).encoder().audio().ok()?;
    ctx.set_rate(rate as i32);
    ctx.set_format(format);
    ctx.set_channel_layout(layout);
    ctx.set_bit_rate(192_000);
    ctx.set_time_base(ff::Rational(1, rate as i32));
    ctx.set_flags(ff::codec::Flags::GLOBAL_HEADER);
    let mut enc = ctx.open().ok()?;
    let n = if enc.frame_size() > 0 { enc.frame_size() as usize } else { 1024 };
    let mut packets = Vec::new();
    let mut pkt = ff::Packet::empty();
    for i in 0..8 {
        let mut frame = ff::frame::Audio::new(format, n, layout);
        frame.set_rate(rate);
        for c in 0..usize::from(channels.clamp(1, 2)) {
            frame.plane_mut::<f32>(c).fill(0.0);
        }
        frame.set_pts(Some((i * n) as i64));
        enc.send_frame(&frame).ok()?;
        while enc.receive_packet(&mut pkt).is_ok() {
            packets.push(pkt.data().map(|d| d.to_vec()).unwrap_or_default());
        }
    }
    packets.into_iter().filter(|p| !p.is_empty()).nth(3)
}

/// Before a `.part` is recovered (`longrec::recover_in`): when the controller says a privacy
/// line was never applied (`mute_from_ms`), zero its device tracks from that moment. Err: the
/// part must not be recovered with that audio in it (it stays a `.part`).
pub fn mute_before_recovery(part: &Path, mute_from_ms: Option<u64>) -> Result<(), String> {
    let Some(ms) = mute_from_ms else { return Ok(()) };
    let Some(sidecar) = read_sidecar(part) else {
        return Err("its device tracks are unknown (no sidecar)".into());
    };
    let from = cutoff_secs(&sidecar, ms);
    let n = zero_device_audio(part, &sidecar.device, from, &silent_frame)?;
    eprintln!("[recall] long recording {}: {n} device-audio frames from {from:.1} s on muted (a privacy change the last engine never applied)", part.display());
    Ok(())
}

/// `RECALL_LONGREC_MUTE_FROM` as unix ms.
pub fn mute_from_env() -> Option<u64> {
    std::env::var(MUTE_FROM_ENV).ok()?.trim().parse().ok()
}

#[cfg(test)]
#[path = "part_mute_tests.rs"]
mod tests;
