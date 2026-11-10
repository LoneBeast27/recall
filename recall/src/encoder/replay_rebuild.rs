//! A replay save cut off mid-write (a kill, a crash, a power cut), rebuilt into its clip at the
//! next start from its packet index (`replay_index`), never parked.
//!
//! - [`rebuild`]: the streams come from the index's header-only MP4 (every sample entry as the
//!   clip had it); every packet that is whole in the `.part` (and not all zeros) is copied, in
//!   order, into a fresh MP4 with its own sample tables. Nothing past the cut is played.
//! - [`recover_entry`]: one save the in-progress list names (a path that is not a replay save's
//!   `.part`, or an index outside Recall's replay-index folder, is never acted on). Finished and
//!   playing (only the rename was lost): renamed into place. Cut off (a torn `moov` included)
//!   with its index: rebuilt into its clip's folder and verified; the cut `.part` then goes to
//!   the Recycle Bin, unless it holds more than its index knew (kept beside the clip). No packet
//!   at all: a diagnostic line and the Recycle Bin. Cut off without an index: kept, logged. A
//!   failure that may pass (a folder that cannot be made, a drive not mounted) keeps it listed.
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use ffmpeg_next as ff;

use super::in_progress::{self, Entry};
use super::part_recover::{layout, Layout};
use super::recycle::{dispose_unrecoverable, layout_summary, recycle};
use super::replay_index::{self, Record};
use crate::naming;

#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Rebuilt {
    pub packets: usize,
    pub video_packets: usize,
    /// End of the last video packet.
    pub seconds: f64,
    /// The `.part` holds data past the last packet taken (its index lagged: a power cut).
    pub part_holds_more: bool,
}

/// No whole video packet in the `.part`: nothing to rebuild.
pub(crate) const NO_VIDEO: &str = "no whole video packet";
/// The index is another save's (a stale entry): nothing here may use or recycle it.
pub(crate) const NOT_ITS_INDEX: &str = "the index belongs to";

/// Rebuild the save cut off in `part` from the index at `index` into `out` (see the module doc).
pub(crate) fn rebuild(part: &Path, index: &Path, out: &Path) -> Result<Rebuilt, String> {
    let (head, records) = replay_index::read(index)?;
    if !in_progress::same_path(&head.part, &part.to_string_lossy()) {
        return Err(format!("{NOT_ITS_INDEX} {}", head.part));
    }
    let _ = ff::init();
    let ictx = ff::format::input(&replay_index::hdr_of(index)).map_err(|e| format!("index header: {e}"))?;
    if ictx.streams().count() != head.time_bases.len() {
        return Err("the index header and its records disagree".into());
    }
    let mut octx = ff::format::output_as(out, "mp4").map_err(|e| format!("open {}: {e}", out.display()))?;
    for (i, ist) in ictx.streams().enumerate() {
        let mut ost = octx.add_stream(None::<ff::Codec>).map_err(|e| e.to_string())?;
        ost.set_parameters(ist.parameters());
        let (num, den) = head.time_bases[i];
        ost.set_time_base(ff::Rational(num, den));
        ost.set_metadata(ist.metadata().to_owned());
        if i == 0 {
            if let Some(tag) = super::mux::mp4_video_tag(ist.parameters().id()) {
                // SAFETY: `ost` is a live stream of `octx` with the codecpar just copied in.
                unsafe { (*(*ost.as_mut_ptr()).codecpar).codec_tag = tag };
            }
        }
    }
    octx.set_metadata(ictx.metadata().to_owned());
    let mut options = ff::Dictionary::new();
    options.set("movflags", "use_metadata_tags");
    octx.write_header_with(options).map_err(|e| format!("write_header: {e}"))?;
    let out_tbs: Vec<ff::Rational> = octx.streams().map(|s| s.time_base()).collect();
    let codec = ictx.stream(0).map(|s| s.parameters().id()).unwrap_or(ff::codec::Id::None);
    let mut file = std::fs::File::open(part).map_err(|e| format!("open {}: {e}", part.display()))?;
    let len = file.metadata().map(|m| m.len()).unwrap_or(0);
    let data_end = super::recycle::data_end(&mut file, len);
    let (mut packets, mut video, mut end_us, mut used) = (0usize, 0usize, 0i64, 0u64);
    // Audio waits for the next video packet that checks out: a torn audio frame at the very tail
    // is dropped, never put in the clip.
    let mut pending: Vec<(Record, Vec<u8>)> = Vec::new();
    let mut stopped = Stop::IndexEnd;
    for r in &records {
        if r.stream as usize >= out_tbs.len() || r.offset + u64::from(r.size) > data_end {
            stopped = Stop::DataEnd;
            break;
        }
        let mut buf = vec![0u8; r.size as usize];
        file.seek(SeekFrom::Start(r.offset)).and_then(|_| file.read_exact(&mut buf)).map_err(|e| format!("read: {e}"))?;
        if buf.iter().all(|b| *b == 0) {
            continue; // a hole a power cut left (an out-of-order flush): only this packet is lost
        }
        if r.stream != 0 {
            pending.push((*r, buf));
            continue;
        }
        if !video_sample_ok(codec, &buf) {
            stopped = Stop::Invalid;
            break;
        }
        for (a, bytes) in pending.drain(..).chain(std::iter::once((*r, buf))) {
            write_packet(&mut octx, &a, &bytes, &head.time_bases, &out_tbs)?;
            packets += 1;
            used = a.offset + u64::from(a.size);
        }
        video += 1;
        end_us = end_us.max(r.pts + r.dur);
    }
    if video == 0 {
        return Err(NO_VIDEO.into());
    }
    octx.write_trailer().map_err(|e| format!("write_trailer: {e}"))?;
    // More than the clip took: the index ended with data still past its last record, or a
    // record inside the data did not check out. Not when the data simply ends (a cut).
    let last_end = records.last().map_or(0, |r| r.offset + u64::from(r.size));
    let part_holds_more = match stopped {
        Stop::DataEnd => false,
        Stop::Invalid => true,
        Stop::IndexEnd => data_end > last_end.max(used),
    };
    Ok(Rebuilt { packets, video_packets: video, seconds: end_us as f64 / 1e6, part_holds_more })
}

/// Why a rebuild stopped taking records.
enum Stop {
    /// Every record was taken (or skipped as a hole).
    IndexEnd,
    /// The next record reaches past the data in the `.part` (the cut).
    DataEnd,
    /// A record inside the data is not a video sample.
    Invalid,
}

fn write_packet(octx: &mut ff::format::context::Output, r: &Record, bytes: &[u8], time_bases: &[(i32, i32)], out_tbs: &[ff::Rational]) -> Result<(), String> {
    let mut pkt = ff::Packet::copy(bytes);
    pkt.set_pts(Some(r.pts));
    pkt.set_dts(Some(r.pts));
    pkt.set_duration(r.dur);
    if r.key {
        pkt.set_flags(ff::packet::Flags::KEY);
    }
    pkt.set_stream(r.stream as usize);
    let (num, den) = time_bases[r.stream as usize];
    pkt.rescale_ts(ff::Rational(num, den), out_tbs[r.stream as usize]);
    pkt.write(octx).map(|_| ()).map_err(|e| format!("write: {e}"))
}

/// A video sample as the muxer writes it: for H.264 and HEVC an exact chain of 4-byte NAL
/// lengths filling it, each NAL with a valid header; for other codecs (AV1), any non-zero bytes.
pub(crate) fn video_sample_ok(codec: ff::codec::Id, buf: &[u8]) -> bool {
    let header_ok = |nal: &[u8]| match codec {
        ff::codec::Id::H264 => nal[0] & 0x80 == 0 && (1..=23).contains(&(nal[0] & 0x1F)),
        ff::codec::Id::HEVC => nal.len() >= 2 && nal[0] & 0x80 == 0 && (nal[0] >> 1) < 48 && nal[1] & 0x07 != 0,
        _ => true,
    };
    if !matches!(codec, ff::codec::Id::H264 | ff::codec::Id::HEVC) {
        return buf.iter().any(|b| *b != 0);
    }
    let mut at = 0usize;
    while at < buf.len() {
        let Some(len) = buf.get(at..at + 4).map(|b| u32::from_be_bytes(b.try_into().unwrap_or_default()) as usize) else { return false };
        let nal = at + 4;
        if len == 0 || nal + len > buf.len() || !header_ok(&buf[nal..nal + len]) {
            return false;
        }
        at = nal + len;
    }
    at == buf.len() && !buf.is_empty()
}

fn with_suffix(path: &Path, suffix: &str) -> PathBuf {
    let mut s = path.as_os_str().to_owned();
    s.push(suffix);
    PathBuf::from(s)
}

/// The index files of a valid `entry` (Recall's own base, `replay_index::is_index_base`), to
/// the Recycle Bin.
fn recycle_index(entry: &Entry, logs: &mut Vec<String>) {
    let Some(base) = entry.index.as_deref().map(PathBuf::from).filter(|b| replay_index::is_index_base(b)) else { return };
    for file in [replay_index::idx_of(&base), replay_index::hdr_of(&base)] {
        if file.exists() {
            if let Err(e) = recycle(&file) {
                logs.push(format!("replay save index {} left where it is: {e}", file.display()));
            }
        }
    }
}

/// Why `entry` is not a replay save Recall wrote (it is then never acted on).
fn invalid(entry: &Entry) -> Option<String> {
    let name = entry.part().file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    if !super::part_recover::is_replay_part(&name) {
        return Some(format!("{} is not a replay save's .part", entry.path));
    }
    match entry.index.as_deref().map(Path::new) {
        Some(base) if !replay_index::is_index_base(base) => Some(format!("its index {} is not in Recall's replay-index folder", base.display())),
        _ => None,
    }
}

/// What became of one listed save: done (unlisted) or to try again at the next start.
enum Outcome {
    Done,
    Retry,
}

/// Recover the replay save `entry` names (see the module doc): a clip made goes into `clips`.
pub(crate) fn recover_entry(entry: &Entry, clips: &mut Vec<PathBuf>, logs: &mut Vec<String>) {
    let part = entry.part();
    let outcome = match invalid(entry) {
        Some(why) => {
            logs.push(format!("in-progress entry {} not acted on: {why}; dropped from the list", entry.path));
            Outcome::Done
        }
        None => recover_valid(entry, &part, clips, logs),
    };
    if let Outcome::Done = outcome {
        if let Err(e) = in_progress::remove(&part) {
            logs.push(format!("replay save {} still listed ({e})", part.display()));
        }
    }
}

fn recover_valid(entry: &Entry, part: &Path, clips: &mut Vec<PathBuf>, logs: &mut Vec<String>) -> Outcome {
    let clip = entry.clip.as_deref().map(PathBuf::from).unwrap_or_else(|| PathBuf::from(entry.path.trim_end_matches(naming::PART_EXT)));
    if !part.exists() {
        if !part.parent().is_some_and(Path::is_dir) {
            // Its folder is gone: a drive not mounted yet. Kept, index and all, for later.
            logs.push(format!("replay save {}: its folder is not there now; kept listed", part.display()));
            return Outcome::Retry;
        }
        if !clip.exists() {
            logs.push(format!("replay save {} was listed but is gone", part.display()));
        }
        recycle_index(entry, logs);
        return Outcome::Done;
    }
    match layout(part) {
        // Whole and playing: only the rename was lost.
        Ok(Layout::Finished) if super::part_recover::plays(part) => {
            let target = naming::recovered_path(part, Path::exists);
            if let Err(e) = std::fs::rename(part, &target) {
                logs.push(format!("replay save {} not renamed now ({e}); kept listed", part.display()));
                return Outcome::Retry;
            }
            let _ = crate::game_folder_rule::record_saved(&target, Some(part));
            logs.push(format!("recovered replay save {} as {}", part.display(), target.display()));
            clips.push(target);
            recycle_index(entry, logs);
            Outcome::Done
        }
        Ok(Layout::NoPackets) => {
            logs.extend(dispose_unrecoverable(part, "no packet was written"));
            if !part.exists() {
                recycle_index(entry, logs);
            }
            Outcome::Done
        }
        _ => rebuild_entry(entry, part, &clip, clips, logs),
    }
}

fn rebuild_entry(entry: &Entry, part: &Path, clip: &Path, clips: &mut Vec<PathBuf>, logs: &mut Vec<String>) -> Outcome {
    let index_len = |b: &Path| std::fs::metadata(replay_index::idx_of(b)).map_or(0, |m| m.len());
    if let (Some(held), Some(base)) = (entry.held_more, entry.index.as_deref().map(Path::new)) {
        if index_len(base) <= held {
            return Outcome::Retry; // nothing new to rebuild from: left alone, no log
        }
    }
    let Some(index) = entry.index.as_deref().map(PathBuf::from).filter(|b| replay_index::idx_of(b).exists()) else {
        // Nothing will change at a later start: kept, logged once.
        logs.push(format!("replay save {} was cut off and has no packet index; kept as it is, not deleted: boxes {}", part.display(), layout_summary(part)));
        return Outcome::Done;
    };
    if let Some(dir) = clip.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    // Our own temp file: a rebuild cut off is redone over it at the next start.
    let tmp = with_suffix(clip, ".rebuild.tmp");
    let rebuilt = rebuild(part, &index, &tmp).and_then(|r| verified(&tmp, r));
    if rebuilt.is_err() && tmp.exists() {
        let _ = recycle(&tmp); // a half-made temp of our own
    }
    let r = match rebuilt {
        Ok(r) => r,
        Err(e) if e == NO_VIDEO => {
            logs.extend(dispose_unrecoverable(part, "no whole video packet by its index"));
            if !part.exists() {
                recycle_index(entry, logs);
            }
            return Outcome::Done;
        }
        Err(e) if e.starts_with(NOT_ITS_INDEX) => {
            // Never right at a later start either: kept, logged once, unlisted.
            logs.push(format!("replay save {} kept as it is: {e}: boxes {}", part.display(), layout_summary(part)));
            return Outcome::Done;
        }
        Err(e) => {
            logs.push(format!("replay save {} not rebuilt now ({e}); kept listed for the next start: boxes {}", part.display(), layout_summary(part)));
            return Outcome::Retry;
        }
    };
    // A clip rebuilt before (from a shorter index) is replaced by this fuller one.
    let target = entry.rebuilt.as_deref().map(PathBuf::from).filter(|p| p.is_file()).unwrap_or_else(|| naming::recovered_path(&naming::part_of(clip), Path::exists));
    if let Err(e) = std::fs::rename(&tmp, &target) {
        logs.push(format!("replay save {} rebuilt but not renamed now ({e}); kept listed", part.display()));
        return Outcome::Retry;
    }
    let _ = crate::game_folder_rule::record_saved(&target, None);
    logs.push(format!("rebuilt replay save {} as {} ({} packets, {:.1} s)", part.display(), target.display(), r.packets, r.seconds));
    clips.push(target.clone());
    if r.part_holds_more {
        // The cut file holds more than this rebuild could take: it stays, listed (never an
        // orphan), and is rebuilt again only if its index grows.
        let (held, rebuilt) = (index_len(&index), target.to_string_lossy().into_owned());
        let _ = in_progress::update(part, |e| (e.held_more, e.rebuilt) = (Some(held), Some(rebuilt)));
        logs.push(format!("replay save {} holds more than its index knew; kept listed beside the clip: boxes {}", part.display(), layout_summary(part)));
        return Outcome::Retry;
    }
    let size = std::fs::metadata(part).map_or(0, |m| m.len());
    match recycle(part) {
        Ok(()) => logs.push(format!("moved {} ({size} B) to the Recycle Bin: rebuilt as {} ({:.1} s)", part.display(), target.display(), r.seconds)),
        Err(e) => logs.push(format!("replay save {} left where it is: {e}", part.display())),
    }
    recycle_index(entry, logs);
    Outcome::Done
}

/// The rebuilt clip demuxes with every video packet the rebuild wrote.
fn verified(clip: &Path, r: Rebuilt) -> Result<Rebuilt, String> {
    let mut input = ff::format::input(&clip).map_err(|e| format!("the rebuilt clip does not open: {e}"))?;
    let played = input.packets().filter(|(s, _)| s.index() == 0).count();
    if played == r.video_packets {
        Ok(r)
    } else {
        Err(format!("the rebuilt clip plays {played} of its {} video packets", r.video_packets))
    }
}

#[cfg(test)]
#[path = "replay_review_tests.rs"]
pub(crate) mod replay_review_tests;
#[cfg(test)]
#[path = "replay_review2_tests.rs"]
mod tests2;
#[cfg(test)]
#[path = "replay_live_tests.rs"]
mod live_tests;
#[cfg(test)]
#[path = "replay_plan_tests.rs"]
mod plan_tests;
