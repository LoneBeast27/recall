//! A replay save cut off mid-write (a kill, a crash, a power cut), rebuilt into its clip at the
//! next start from its packet index (`replay_index`), never parked.
//!
//! - [`rebuild`]: the streams come from the index's header-only MP4 (every sample entry as the
//!   clip had it); every packet that is whole in the `.part` (and not all zeros) is copied, in
//!   order, into a fresh MP4 with its own sample tables. Nothing past the cut is played.
//! - [`recover_entry`]: one save the in-progress list names. Finished (only the rename was lost):
//!   renamed into place. Cut off with its index: rebuilt into its clip's folder; the cut `.part`
//!   and the index then go to the Recycle Bin. No packet at all: a diagnostic line and the
//!   Recycle Bin. Cut off without an index: kept as it is, with a diagnostic line (never recycled).
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use ffmpeg_next as ff;

use super::in_progress::{self, Entry};
use super::part_recover::{layout, Layout};
use super::recycle::{dispose_unrecoverable, layout_summary, recycle};
use super::replay_index;
use crate::naming;

#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Rebuilt {
    pub packets: usize,
    pub video_packets: usize,
    /// End of the last video packet.
    pub seconds: f64,
}

/// No whole video packet in the `.part`: nothing to rebuild.
pub(crate) const NO_VIDEO: &str = "no whole video packet";

/// Rebuild the save cut off in `part` from the index at `index` into `out` (see the module doc).
pub(crate) fn rebuild(part: &Path, index: &Path, out: &Path) -> Result<Rebuilt, String> {
    let (head, records) = replay_index::read(index)?;
    if !head.part.eq_ignore_ascii_case(&part.to_string_lossy()) {
        return Err(format!("the index belongs to {}", head.part));
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
    let mut file = std::fs::File::open(part).map_err(|e| format!("open {}: {e}", part.display()))?;
    let len = file.metadata().map(|m| m.len()).unwrap_or(0);
    let (mut packets, mut video, mut end_us) = (0usize, 0usize, 0i64);
    let mut buf = Vec::new();
    for r in &records {
        if r.offset + u64::from(r.size) > len || r.stream as usize >= out_tbs.len() {
            break;
        }
        buf.resize(r.size as usize, 0);
        file.seek(SeekFrom::Start(r.offset)).and_then(|_| file.read_exact(&mut buf)).map_err(|e| format!("read: {e}"))?;
        if buf.iter().all(|b| *b == 0) {
            break; // a power cut's zero-filled tail
        }
        let mut pkt = ff::Packet::copy(&buf);
        pkt.set_pts(Some(r.pts));
        pkt.set_dts(Some(r.pts));
        pkt.set_duration(r.dur);
        if r.key {
            pkt.set_flags(ff::packet::Flags::KEY);
        }
        pkt.set_stream(r.stream as usize);
        let (num, den) = head.time_bases[r.stream as usize];
        pkt.rescale_ts(ff::Rational(num, den), out_tbs[r.stream as usize]);
        pkt.write(&mut octx).map_err(|e| format!("write: {e}"))?;
        packets += 1;
        if r.stream == 0 {
            video += 1;
            end_us = end_us.max(r.pts + r.dur);
        }
    }
    if video == 0 {
        return Err(NO_VIDEO.into());
    }
    octx.write_trailer().map_err(|e| format!("write_trailer: {e}"))?;
    Ok(Rebuilt { packets, video_packets: video, seconds: end_us as f64 / 1e6 })
}

fn with_suffix(path: &Path, suffix: &str) -> PathBuf {
    let mut s = path.as_os_str().to_owned();
    s.push(suffix);
    PathBuf::from(s)
}

/// The index files of `entry` (a claimed copy), to the Recycle Bin.
fn recycle_index(entry: &Entry, logs: &mut Vec<String>) {
    let Some(base) = entry.index.as_deref().map(PathBuf::from) else { return };
    for file in [replay_index::idx_of(&base), replay_index::hdr_of(&base)] {
        if file.exists() {
            if let Err(e) = recycle(&file) {
                logs.push(format!("replay save index {} left where it is: {e}", file.display()));
            }
        }
    }
}

/// Recover the replay save `entry` names (see the module doc): a clip made goes into `clips`.
pub(crate) fn recover_entry(entry: &Entry, clips: &mut Vec<PathBuf>, logs: &mut Vec<String>) {
    let part = entry.part();
    let clip = entry.clip.as_deref().map(PathBuf::from).unwrap_or_else(|| PathBuf::from(entry.path.trim_end_matches(naming::PART_EXT)));
    let done = |logs: &mut Vec<String>| {
        if let Err(e) = in_progress::remove(&part) {
            logs.push(format!("replay save {} still listed ({e})", part.display()));
        }
    };
    if !part.exists() {
        if !clip.exists() {
            logs.push(format!("replay save {} was listed but is gone", part.display()));
        }
        recycle_index(entry, logs);
        return done(logs);
    }
    match layout(&part) {
        Ok(Layout::Finished) => {
            // Whole: only the rename was lost.
            let target = naming::recovered_path(&part, Path::exists);
            match std::fs::rename(&part, &target) {
                Ok(()) => {
                    let _ = crate::game_folder_rule::record_saved(&target, Some(&part));
                    logs.push(format!("recovered replay save {} as {}", part.display(), target.display()));
                    clips.push(target);
                }
                Err(e) => logs.push(format!("replay save {} left as it is: not renamed: {e}", part.display())),
            }
            recycle_index(entry, logs);
        }
        Ok(Layout::NoPackets) => {
            logs.extend(dispose_unrecoverable(&part, "no packet was written"));
            recycle_index(entry, logs);
        }
        Ok(Layout::Unindexed) | Err(_) => rebuild_entry(entry, &part, &clip, clips, logs),
    }
    done(logs)
}

fn rebuild_entry(entry: &Entry, part: &Path, clip: &Path, clips: &mut Vec<PathBuf>, logs: &mut Vec<String>) {
    let Some(index) = entry.index.as_deref().map(PathBuf::from).filter(|b| replay_index::idx_of(b).exists()) else {
        logs.push(format!("replay save {} was cut off and has no packet index; kept as it is, not deleted: boxes {}", part.display(), layout_summary(part)));
        return;
    };
    if let Some(dir) = clip.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    // Our own temp file: a rebuild cut off is redone over it at the next start.
    let tmp = with_suffix(clip, ".rebuild.tmp");
    let rebuilt = rebuild(part, &index, &tmp);
    if rebuilt.is_err() && tmp.exists() {
        let _ = recycle(&tmp); // a half-made temp of our own
    }
    match rebuilt {
        Ok(r) => {
            let target = naming::recovered_path(&naming::part_of(clip), Path::exists);
            if let Err(e) = std::fs::rename(&tmp, &target) {
                return logs.push(format!("replay save {} rebuilt but not renamed ({e}); the cut-off file is kept", part.display()));
            }
            let _ = crate::game_folder_rule::record_saved(&target, None);
            logs.push(format!("rebuilt replay save {} as {} ({} packets, {:.1} s)", part.display(), target.display(), r.packets, r.seconds));
            clips.push(target);
            if let Err(e) = recycle(part) {
                logs.push(format!("replay save {} left where it is: {e}", part.display()));
            }
            recycle_index(entry, logs);
        }
        Err(e) if e == NO_VIDEO => {
            logs.extend(dispose_unrecoverable(part, "no whole video packet"));
            recycle_index(entry, logs);
        }
        Err(e) => logs.push(format!("replay save {} not rebuilt ({e}); kept as it is, not deleted: boxes {}", part.display(), layout_summary(part))),
    }
}
