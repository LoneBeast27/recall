//! Programmatic exports of recorded clips.
//!
//! * [`export_cfr`] re-encodes a whole VFR clip to CFR with NVIDIA's H.264
//!   encoder. Output ticks use a `1/fps` timebase and each tick receives the
//!   latest decoded frame whose source timestamp is not later than that tick;
//!   captured frames are repeated or dropped, never synthesized.
//! * [`export_edited`] applies a nondestructive [`EditPlan`]: either the same
//!   CFR re-encode over the kept ranges (with per-range speed, an optional file
//!   size cap and downscale) or a lossless stream-copy trim whose cuts start on
//!   keyframes.
//!
//! Every export writes a hidden temporary file beside the destination and
//! publishes it with a rename, so a failed export never leaves a partial file.

mod audio_mux;
pub(crate) mod bad_audio;
mod cfr;
mod edited;
pub(crate) mod edit_pcm;
pub(crate) mod keyframes;
mod lossless;
pub(crate) mod mix_limit;
mod paths;
pub mod project;
mod size_cap;
mod sources;
pub(crate) mod tempo;
pub(crate) mod timeline;
mod video_encoder;

#[cfg(test)]
mod bad_packet_tests;
#[cfg(test)]
mod edited_tests;
#[cfg(test)]
mod lossless_tests;
#[cfg(test)]
mod mix_tests;
#[cfg(test)]
mod stamp_jitter_tests;
#[cfg(test)]
mod test_media;

use ffmpeg_next as ff;

use crate::edit_model::{EditPlan, ExportMode, ExportSettings};
pub use project::{parse_project, EditedExport};
use video_encoder::VideoBackend;

#[derive(Debug, Clone)]
pub struct ExportRequest {
    pub input: String,
    pub output: String,
    pub fps: u32,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ExportResult {
    pub frames: u64,
    pub seconds: f64,
}

static PROGRESS: std::sync::OnceLock<fn(u64, u64)> = std::sync::OnceLock::new();

/// Where edited re-encodes report (frames written, frames planned). Only a
/// standalone `--export-edit` sets one; the recorder's own exports stay quiet.
pub fn set_progress_sink(sink: fn(u64, u64)) {
    let _ = PROGRESS.set(sink);
}

pub(crate) fn report_progress(done: u64, total: u64) {
    if let Some(sink) = PROGRESS.get() {
        sink(done, total);
    }
}

pub(crate) fn receive_is_drained(error: &ff::Error) -> bool {
    match error {
        ff::Error::Eof => true,
        ff::Error::Other { errno } => *errno == ff::error::EAGAIN,
        _ => false,
    }
}

/// Re-encode a whole clip to CFR at `req.fps`; every audio stream is remuxed.
pub fn export_cfr(req: &ExportRequest) -> Result<ExportResult, String> {
    paths::publish(req, |input, temp| cfr::export_to_temp(input, temp, req.fps))
}

/// Export a nondestructive edit plan. Re-encode mode produces gapless CFR video
/// plus one AAC stream per source audio track; lossless mode stream-copies.
pub fn export_edited(
    req: &ExportRequest,
    plan: &EditPlan,
    settings: &ExportSettings,
) -> Result<EditedExport, String> {
    export_edited_with(req, plan, settings, VideoBackend::Nvenc)
}

/// [`export_edited`] with an explicit video encoder; tests pass the CPU encoder.
pub(crate) fn export_edited_with(
    req: &ExportRequest,
    plan: &EditPlan,
    settings: &ExportSettings,
    backend: VideoBackend,
) -> Result<EditedExport, String> {
    settings.validate(plan)?;
    paths::publish(req, |input, temp| match settings.mode {
        ExportMode::Reencode => edited::export(input, temp, req.fps, plan, settings, backend),
        ExportMode::Lossless => lossless::export(input, temp, plan),
    })
}

#[cfg(test)]
mod tests {
    use super::ff;

    #[test]
    fn receive_classifier_only_accepts_eagain_and_eof() {
        assert!(super::receive_is_drained(&ff::Error::Eof));
        assert!(super::receive_is_drained(&ff::Error::Other {
            errno: ff::error::EAGAIN,
        }));
        assert!(!super::receive_is_drained(&ff::Error::InvalidData));
        assert!(!super::receive_is_drained(&ff::Error::Other {
            errno: ff::error::EIO,
        }));
    }
}
