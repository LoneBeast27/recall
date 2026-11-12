//! The clip's stream setup (shared by the clip and its packet index's header-only MP4), the
//! index records, and the one packet write the mux uses (part of `mux`).
use std::path::Path;

use ffmpeg_next as ff;

use super::super::replay_index::{self, Record};
use super::{mp4_video_tag, track_metadata, AacTrack, OwnedParams};

/// An MP4 output at `path` with the clip's streams (video from `params`, one AAC stream per
/// track with its title, the audio-sources tag) and its header written with `movflags`.
pub(crate) fn open_output(path: &Path, params: &OwnedParams, backend: &str, tracks: &[AacTrack], movflags: &str) -> Result<ff::format::context::Output, String> {
    // Explicit format: the in-progress `.part` name has no usable extension.
    let mut octx = ff::format::output_as(path, "mp4").map_err(|e| format!("open {}: {e}", path.display()))?;
    let vcodec = ff::encoder::find_by_name(backend);
    let mut ost = octx.add_stream(vcodec).map_err(|e| e.to_string())?;
    ost.set_parameters(params.0.clone());
    ost.set_time_base(ff::Rational(1, 1_000_000));
    if let Some(tag) = mp4_video_tag(params.0.id()) {
        // SAFETY: `ost` is a live stream of `octx`; avformat_new_stream
        // allocated its codecpar, which set_parameters just filled.
        unsafe { (*(*ost.as_mut_ptr()).codecpar).codec_tag = tag };
    }
    for t in tracks {
        let mut ast = octx.add_stream(ff::encoder::find(ff::codec::Id::AAC)).map_err(|e| e.to_string())?;
        ast.set_parameters(&t.encoder);
        ast.set_time_base(ff::Rational(1, t.rate as i32));
        ast.set_metadata(track_metadata(t.title));
    }
    // Each track's source, for the editor (app tracks start out of the mix). MP4 keeps custom
    // tags only at file level, and only with use_metadata_tags.
    let mut file_tags = ff::Dictionary::new();
    let sources: Vec<&str> = tracks.iter().map(|t| t.source).collect();
    file_tags.set(crate::edit_model::AUDIO_SOURCES_TAG, &serde_json::to_string(&sources).unwrap_or_default());
    // The rate the encoder was opened at: the stream is variable rate, so its average says
    // little about it (`clip_rate`).
    // SAFETY: a live, owned AVCodecParameters (copied from the encoder, framerate included).
    let rate = unsafe { (*params.0.as_ptr()).framerate };
    if let Some(fps) = crate::clip_rate::tag_value(rate.num, rate.den) {
        file_tags.set(crate::clip_rate::RECORDED_FPS_TAG, &fps);
    }
    octx.set_metadata(file_tags);
    let mut options = ff::Dictionary::new();
    options.set("movflags", movflags);
    octx.write_header_with(options).map_err(|e| format!("write_header: {e}"))?;
    Ok(octx)
}

/// Start the packet index for the save into `part` at `base` (`replay_index`): the
/// header-only fragmented MP4 (every sample entry, written at its header), the head and `plan`.
pub(crate) fn begin(base: &Path, part: &Path, params: &OwnedParams, backend: &str, tracks: &[AacTrack], plan: Option<&[Record]>) -> Result<replay_index::Writer, String> {
    let time_bases: Vec<(i32, i32)> = std::iter::once((1, 1_000_000)).chain(tracks.iter().map(|t| (1, t.rate as i32))).collect();
    let header = |hdr: &Path| {
        // Dropped without a trailer: `empty_moov` wrote ftyp + moov at the header; closing flushes it.
        open_output(hdr, params, backend, tracks, "frag_keyframe+empty_moov+use_metadata_tags").map(drop)
    };
    replay_index::begin(base, part, header, &time_bases, plan)
}

/// How the muxer writes a video packet into a plain MP4: unchanged, or with its Annex B start
/// codes turned into 4-byte lengths (`movenc.c`: H.264 when its extradata is not `avcC`,
/// HEVC when its extradata is Annex B; `ff_avc_parse_nal_units`, `ff_hevc_annexb2mp4` with no
/// parameter-set filtering). None: sizes cannot be predicted (AV1 drops OBUs; anything else).
fn video_rewrite(params: &OwnedParams) -> Option<bool> {
    // SAFETY: a live, owned AVCodecParameters; extradata is valid for its size.
    let extra = unsafe {
        let p = &*params.0.as_ptr();
        if p.extradata.is_null() || p.extradata_size <= 0 { Vec::new() } else { std::slice::from_raw_parts(p.extradata, p.extradata_size as usize).to_vec() }
    };
    match params.0.id() {
        ff::codec::Id::H264 => Some(!extra.is_empty() && extra[0] != 1),
        ff::codec::Id::HEVC => Some(extra.len() > 6 && (extra[..3] == [0, 0, 1] || extra[..4] == [0, 0, 0, 1])),
        _ => None,
    }
}

/// The first `00 00 01` at or after `from` (one zero earlier when it is a 4-byte code), else the
/// end: `ff_nal_find_startcode`.
fn find_startcode(buf: &[u8], from: usize) -> usize {
    let mut i = from;
    let at = loop {
        let Some(zero) = next_zero(buf, i) else { break buf.len() };
        if buf.get(zero + 1) == Some(&0) && buf.get(zero + 2) == Some(&1) {
            break zero;
        }
        i = zero + 1;
    };
    if at < buf.len() && at > from && buf[at - 1] == 0 { at - 1 } else { at }
}

/// The first zero byte at or after `from`, eight bytes at a time (compressed video holds few
/// zeros: this is what keeps planning a long save cheap).
fn next_zero(buf: &[u8], from: usize) -> Option<usize> {
    const LO: u64 = 0x0101_0101_0101_0101;
    const HI: u64 = 0x8080_8080_8080_8080;
    let mut i = from;
    while i + 8 <= buf.len() {
        let word = u64::from_le_bytes(buf[i..i + 8].try_into().unwrap_or_default());
        if word.wrapping_sub(LO) & !word & HI != 0 {
            break;
        }
        i += 8;
    }
    buf.get(i..)?.iter().position(|b| *b == 0).map(|j| i + j)
}

/// What `ff_avc_parse_nal_units` writes for `buf`: each NAL as a 4-byte length and its bytes.
pub(crate) fn annexb_to_lengths_size(buf: &[u8]) -> usize {
    let (end, mut size) = (buf.len(), 0);
    let mut start = find_startcode(buf, 0);
    loop {
        while start < end {
            let zero = buf[start] == 0;
            start += 1;
            if !zero {
                break;
            }
        }
        if start >= end {
            return size;
        }
        let nal_end = find_startcode(buf, start);
        size += 4 + nal_end - start;
        start = nal_end;
    }
}

#[cfg(test)]
thread_local! {
    /// Tests: the plan is off by one byte from this record on (a mid-mux mismatch).
    pub(crate) static PLAN_SKEW: std::cell::Cell<Option<usize>> = const { std::cell::Cell::new(None) };
}

/// The planned record of every packet, in write order, from `start` (the `mdat` payload); None
/// when the video's sizes cannot be predicted (then every record is written through) or a
/// spilled packet cannot be read. Items: (stream, key, bytes, pts, duration).
pub(crate) fn plan<'a>(params: &OwnedParams, start: u64, packets: impl Iterator<Item = (usize, bool, &'a super::Bytes<'a>, i64, i64)>) -> Option<Vec<Record>> {
    let rewrite = video_rewrite(params)?;
    let mut at = start;
    let mut out = Vec::new();
    for (i, (stream, key, bytes, pts, dur)) in packets.enumerate() {
        #[allow(unused_mut)]
        let mut size = match bytes {
            super::Bytes::Audio(b) => b.len(),
            super::Bytes::Video(p) if !rewrite => p.len(),
            super::Bytes::Video(p) => annexb_to_lengths_size(&p.read().ok()?),
        } as u32;
        #[cfg(test)]
        if PLAN_SKEW.with(std::cell::Cell::get) == Some(i) {
            size += 1;
        }
        let _ = i;
        out.push(Record { stream: stream as u8, key, size, offset: at, pts, dur });
        at += u64::from(size);
    }
    Some(out)
}

/// Hand what the muxer has buffered to Windows (`avio_flush`): a record written through after
/// it never claims bytes a process kill would lose.
pub(crate) fn hand_over(octx: &mut ff::format::context::Output) {
    // SAFETY: the output context is open and owns a live AVIO context (`pb`).
    unsafe {
        let pb = (*octx.as_mut_ptr()).pb;
        if !pb.is_null() {
            ff::ffi::avio_flush(pb);
        }
    }
}

/// Where the next byte goes in the output (its logical position, buffer included).
pub(crate) fn tell(octx: &mut ff::format::context::Output) -> u64 {
    // SAFETY: the output context is open and owns a live AVIO context (`pb`); a zero seek
    // from the current position only reads it back.
    unsafe {
        let pb = (*octx.as_mut_ptr()).pb;
        if pb.is_null() {
            return 0;
        }
        ff::ffi::avio_seek(pb, 0, 1).max(0) as u64
    }
}

/// One written packet's record (`offset`..`end`: where the muxer put its sample).
pub(crate) fn record(stream: usize, key: bool, offset: u64, end: u64, pts: i64, dur: i64) -> Record {
    Record { stream: stream as u8, key, size: end.saturating_sub(offset) as u32, offset, pts, dur }
}

#[cfg(test)]
thread_local! {
    /// Tests: mux through the interleaver, as before (the layout comparison).
    pub(crate) static INTERLEAVED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Write one packet: in the order given (`write_frame`).
pub(crate) fn write_packet(pkt: &ff::Packet, octx: &mut ff::format::context::Output) -> Result<(), ff::Error> {
    #[cfg(test)]
    if INTERLEAVED.with(std::cell::Cell::get) {
        return pkt.write_interleaved(octx);
    }
    pkt.write(octx).map(|_| ())
}
