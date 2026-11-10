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
    octx.set_metadata(file_tags);
    let mut options = ff::Dictionary::new();
    options.set("movflags", movflags);
    octx.write_header_with(options).map_err(|e| format!("write_header: {e}"))?;
    Ok(octx)
}

/// Start the packet index for the save into `part` at `base` (`replay_index`): the
/// header-only fragmented MP4 (every sample entry, written at its header) and the head.
pub(crate) fn begin(base: &Path, part: &Path, params: &OwnedParams, backend: &str, tracks: &[AacTrack]) -> Result<replay_index::Writer, String> {
    let time_bases: Vec<(i32, i32)> = std::iter::once((1, 1_000_000)).chain(tracks.iter().map(|t| (1, t.rate as i32))).collect();
    let header = |hdr: &Path| {
        // Dropped without a trailer: `empty_moov` wrote ftyp + moov at the header; closing flushes it.
        open_output(hdr, params, backend, tracks, "frag_keyframe+empty_moov+use_metadata_tags").map(drop)
    };
    replay_index::begin(base, part, header, &time_bases)
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
