//! Quiet-tail saves: a fresh keyframe-only NVENC still, encoded on the saver
//! thread with the live encoder's codec and quality.

use super::mux::OwnedParams;
use super::saver::RawFrame;
use super::settings::{Codec, Quality};
use super::replay::Replay;
use super::{open_encoder, Config, EncStats};
use crate::ring::EncodedPacket;

/// Settings a quiet-tail still is encoded with: the live encoder's.
#[derive(Clone, Copy)]
pub(crate) struct StillSpec {
    pub fps: u32,
    pub bitrate: usize,
    pub codec: Codec,
    pub quality: Quality,
}

/// Encode one raw frame as a fresh keyframe-only NVENC stream lasting
/// `seconds`. Runs with its own session; the live encoder and its replay ring
/// are untouched. Also returns the encoder name used (H.264 if the live codec
/// cannot open a second time).
pub(super) fn encode_still(
    frame: &RawFrame,
    spec: StillSpec,
    seconds: u32,
) -> Result<(Vec<EncodedPacket>, OwnedParams, &'static str), String> {
    let (width, height, rgba) = frame;
    let cfg = Config {
        fps: spec.fps,
        buffer_secs: seconds.max(1),
        bitrate: spec.bitrate,
        codec: spec.codec,
        quality: spec.quality,
        json: false,
        notes: Vec::new(),
    };
    let (mut still, _) = open_encoder(&cfg, spec.codec, *width, *height, None)?;
    let stats = EncStats::default();
    let mut replay = Replay::new(&cfg, None);
    replay.begin(&still);
    still.send_rgba(0, *width, rgba)?;
    still.drain(&stats, &mut replay)?;
    still
        .encoder
        .send_eof()
        .map_err(|e| format!("NVENC still flush: {e}"))?;
    still.drain(&stats, &mut replay)?;
    let want_us = seconds as u64 * 1_000_000;
    Ok((
        replay.ring.save_window_at(want_us, want_us),
        still.params.clone(),
        still.backend,
    ))
}
