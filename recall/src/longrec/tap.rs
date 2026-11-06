//! The three hooks the live pipeline calls for the long recording. Each is one
//! relaxed atomic load when no recording is running; while one is, a packet
//! costs an `Arc` clone + `try_send`, and an audio chunk one `Vec` copy +
//! `try_send`. Nothing blocks: a full queue drops the item (video then resyncs
//! at the next keyframe; audio is re-aligned by wall clock).
//!
//! Call sites (the only lines outside this module):
//! - `encoder::run`, after the LIVE encoder opens: [`on_encoder_open`]
//! - `encoder::run`, after a fatal encode error: [`on_encoder_failed`]
//! - `Enc::drain`, for each packet before it enters the ring: [`tap_packet`]
//! - `TrackRing::push_capture`, for each captured chunk: [`tap_audio`]
//!
//! Every packet carries the encoder generation it came from, so a stream
//! change is seen in order even when the `EncoderReopened` notice is dropped.

use std::cell::Cell;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering::Relaxed};
use std::sync::mpsc::SyncSender;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Instant;

use ffmpeg_next as ff;

use super::Data;

static ACTIVE: AtomicBool = AtomicBool::new(false);
/// A video packet could not be queued since the writer last looked.
static GAP: AtomicBool = AtomicBool::new(false);
static GENERATION: AtomicU64 = AtomicU64::new(0);
static PARAMS: Mutex<Option<ff::codec::Parameters>> = Mutex::new(None);
/// (generation, reason) of a live encoder that failed for good. Polled by the
/// writer ([`failure_of`]), so unlike a queued notice it cannot be dropped.
static FAILED: Mutex<Option<(u64, String)>> = Mutex::new(None);
static SENDER: OnceLock<SyncSender<Data>> = OnceLock::new();

thread_local! {
    /// Set on the thread that opened the live encoder. The saver's one-off
    /// still encoder shares `Enc::drain` but never calls `on_encoder_open`,
    /// so its packets are not tapped.
    static LIVE_ENCODER: Cell<bool> = const { Cell::new(false) };
}

/// Live encoder (re)opened: keep its codec parameters for the next recording
/// and tell a running one that the stream changed.
pub fn on_encoder_open(encoder: &ff::encoder::Video) {
    LIVE_ENCODER.with(|live| live.set(true));
    *PARAMS.lock().unwrap_or_else(|p| p.into_inner()) = Some(ff::codec::Parameters::from(encoder));
    let generation = GENERATION.fetch_add(1, Relaxed) + 1;
    if ACTIVE.load(Relaxed) {
        if let Some(tx) = SENDER.get() {
            let _ = tx.try_send(Data::EncoderReopened { generation });
        }
    }
}

/// The live encoder failed and will not reopen at this capture size: a
/// running recording of its stream ends (`recording_failed`, file kept).
pub fn on_encoder_failed(reason: &str) {
    on_encoder_failed_at(GENERATION.load(Relaxed), reason);
}

pub(super) fn on_encoder_failed_at(generation: u64, reason: &str) {
    *FAILED.lock().unwrap_or_else(|p| p.into_inner()) = Some((generation, reason.to_string()));
}

/// Why live encoder `generation` failed, if it did.
pub(super) fn failure_of(generation: u64) -> Option<String> {
    FAILED.lock().unwrap_or_else(|p| p.into_inner()).as_ref().filter(|(g, _)| *g == generation).map(|(_, r)| r.clone())
}

/// One encoded live packet (capture-clock µs PTS).
pub fn tap_packet(pts_us: u64, keyframe: bool, data: &Arc<[u8]>) {
    if !ACTIVE.load(Relaxed) || !LIVE_ENCODER.with(Cell::get) {
        return;
    }
    let Some(tx) = SENDER.get() else { return };
    let generation = GENERATION.load(Relaxed);
    if tx.try_send(Data::Video { pts_us, keyframe, data: Arc::clone(data), generation }).is_err() {
        GAP.store(true, Relaxed);
    }
}

/// One captured audio chunk (interleaved f32 in the track's format).
/// `silence` = the chunk enters the replay ring as silence (muted mic).
pub fn tap_audio(track: &'static str, samples: &[f32], silence: bool) {
    if !ACTIVE.load(Relaxed) || samples.is_empty() {
        return;
    }
    let Some(tx) = SENDER.get() else { return };
    let samples = tap_part(samples);
    let samples = if silence { vec![0.0; samples.len()] } else { samples.to_vec() };
    let _ = tx.try_send(Data::Audio { track, samples, at: Instant::now() });
}

pub(super) fn install_sender(tx: SyncSender<Data>) {
    let _ = SENDER.set(tx);
}

pub(super) fn set_active(on: bool) {
    if on {
        GAP.store(false, Relaxed);
    }
    ACTIVE.store(on, Relaxed);
}

/// Whether video packets were dropped since the last call.
pub(super) fn take_gap() -> bool {
    GAP.swap(false, Relaxed)
}

/// The live encoder's parameters, if it has opened.
pub(super) fn video_params() -> Option<ff::codec::Parameters> {
    PARAMS.lock().unwrap_or_else(|p| p.into_inner()).clone()
}

pub(super) fn generation() -> u64 {
    GENERATION.load(Relaxed)
}

/// At most this many samples of one chunk go to the long recording (5 s of the engine's
/// fixed 48 kHz stereo). Placement is by arrival time, so the writer pads anything before
/// the kept part with silence in small blocks (`Machine::push_audio`).
pub(super) const TAP_MAX_SAMPLES: usize = 5 * 48_000 * 2;

/// The part of a chunk the long recording gets: the newest [`TAP_MAX_SAMPLES`], whole frames.
pub(super) fn tap_part(samples: &[f32]) -> &[f32] {
    &samples[samples.len().saturating_sub(TAP_MAX_SAMPLES)..]
}

#[cfg(test)]
mod tests {
    use super::{tap_part, TAP_MAX_SAMPLES};

    /// 2026-11-07 audit T1: a resume's silence block was copied whole into the writer queue.
    #[test]
    fn a_long_silence_run_reaches_the_recording_as_its_newest_five_seconds() {
        let long: Vec<f32> = (0..3 * TAP_MAX_SAMPLES + 2).map(|i| i as f32).collect();
        let part = tap_part(&long);
        assert_eq!(part.len(), TAP_MAX_SAMPLES);
        assert_eq!(part.last(), long.last(), "the newest samples are kept");
        assert_eq!(part.len() % 2, 0, "whole stereo frames");
        let short = [0.25f32; 960];
        assert_eq!(tap_part(&short).len(), 960, "a normal packet goes through whole");
    }
}
