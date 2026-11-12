//! The recorder's per-track sources tag (`edit_model::AUDIO_SOURCES_TAG`) through an export:
//! the output gets a tag for its own audio tracks, in its own order, so a re-opened export
//! still starts with the right mix switches. MP4 keeps the file-level tag only with
//! `movflags=use_metadata_tags`.
//!
//! This is the one header writer for every export (edited, lossless, CFR): it is always
//! `+faststart` (at the trailer the muxer moves `moov` ahead of `mdat`, one in-place shift of
//! the temp file, so chat apps and browsers can play a shared export before all of it loads).

use ffmpeg_next as ff;

use crate::edit_model::{export_sources, AudioOutput, AUDIO_SOURCES_TAG};

/// The tag for `outputs` (built from the input `ictx`), or `None` for an untagged input.
pub(super) fn for_outputs(ictx: &ff::format::context::Input, audio_indices: &[usize], outputs: &[AudioOutput]) -> Option<Vec<String>> {
    export_sources(&crate::editor_media::audio_sources(ictx), audio_indices, outputs)
}

/// The input's own tag, unchanged, for an export that keeps every track as it is.
pub(super) fn unchanged(ictx: &ff::format::context::Input) -> Option<Vec<String>> {
    let sources = crate::editor_media::audio_sources(ictx);
    (!sources.is_empty()).then_some(sources)
}

/// Copies the input's file-level tags (dropping its sources tag, and its recorded-rate tag
/// unless `same_video`: a lossless export keeps the source's video packets and rate), adds `sources` for the
/// output's tracks, and writes the header: `+faststart`, plus `+use_metadata_tags` when a
/// sources tag is written.
pub(super) fn write_header(octx: &mut ff::format::context::Output, ictx: &ff::format::context::Input, sources: Option<Vec<String>>, same_video: bool) -> Result<(), String> {
    let mut tags = ff::Dictionary::new();
    for (key, value) in ictx.metadata().iter() {
        // A re-encoded export has its own constant rate: the recording's tag would misreport it.
        if key != AUDIO_SOURCES_TAG && (same_video || key != crate::clip_rate::RECORDED_FPS_TAG) {
            tags.set(key, value);
        }
    }
    if let Some(sources) = &sources {
        tags.set(AUDIO_SOURCES_TAG, &serde_json::to_string(sources).unwrap_or_default());
    }
    octx.set_metadata(tags);
    let mut options = ff::Dictionary::new();
    options.set("movflags", movflags(sources.is_some()));
    octx.write_header_with(options).map(|_| ()).map_err(|e| format!("write output header: {e}"))
}

/// The export's `movflags`.
fn movflags(tagged: bool) -> &'static str {
    if tagged { "+faststart+use_metadata_tags" } else { "+faststart" }
}
