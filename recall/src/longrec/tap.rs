//! The three hooks the live pipeline calls for the long recording. Each is one
//! relaxed atomic load when no recording is running; while one is, a packet
//! costs an `Arc` clone + `try_send`, and an audio chunk one `Vec` copy +
//! `try_send`. Nothing blocks: a full queue drops the item (video then resyncs
//! at the next keyframe; audio is re-aligned by wall clock).
//!
//! Call sites (the only lines outside this module):
//! - `encoder::run`, after the LIVE encoder opens: [`on_encoder_open`]
//! - `Enc::drain`, for each packet before it enters the ring: [`tap_packet`]
//! - `TrackRing::push_capture`, for each captured chunk: [`tap_audio`]

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

/// One encoded live packet (capture-clock µs PTS).
pub fn tap_packet(pts_us: u64, keyframe: bool, data: &Arc<[u8]>) {
    if !ACTIVE.load(Relaxed) || !LIVE_ENCODER.with(Cell::get) {
        return;
    }
    let Some(tx) = SENDER.get() else { return };
    if tx.try_send(Data::Video { pts_us, keyframe, data: Arc::clone(data) }).is_err() {
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
