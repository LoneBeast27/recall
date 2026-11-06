//! The three hooks the live pipeline calls for the long recording. Each is one
//! relaxed atomic load when no recording is running; while one is, a packet
//! costs an `Arc` clone + `try_send`, and an audio chunk one `Vec` copy +
//! `try_send`. Nothing blocks: a full queue drops the item (video then resyncs
//! at the next keyframe; audio is re-aligned by wall clock).
//!
//! Call sites (the only lines outside this module):
//! - `encoder::run`, after the LIVE encoder opens: [`on_encoder_open`]
//! - `encoder::live` `tick`, once encoder recovery is spent (status `failed`): [`on_encoder_failed`]
//! - `encoder::live` `frame`, on an encode error (before the recovery): [`on_encode_error`]
//! - `Enc::drain`, for each packet before it enters the ring: [`tap_packet`]
//! - `TrackRing::push_capture`, for each captured chunk: [`tap_audio`]
//!
//! Every packet carries the encoder generation it came from, so a stream
//! change is seen in order even when the `EncoderReopened` notice is dropped.

use std::cell::Cell;
use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering::Relaxed};
use std::sync::mpsc::SyncSender;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Instant;

use ffmpeg_next as ff;

use super::machine::Cause;
use super::Data;

static ACTIVE: AtomicBool = AtomicBool::new(false);
/// A video packet could not be queued since the writer last looked.
static GAP: AtomicBool = AtomicBool::new(false);
static GENERATION: AtomicU64 = AtomicU64::new(0);
/// The newest live encoders' settings by generation (bumped under this lock), so
/// a part opens with the settings of the encoder its packets come from.
static STREAMS: Mutex<VecDeque<Stream>> = Mutex::new(VecDeque::new());
const KEPT_STREAMS: usize = 8;
/// (generation, reason) of a live encoder that failed for good. Polled by the
/// writer ([`failure_of`]), so unlike a queued notice it cannot be dropped.
static FAILED: Mutex<Option<(u64, String)>> = Mutex::new(None);
/// (generation, when) of a live encoder's encode error (the first one): a same-header
/// recovery's freeze counts from it. Polled by the writer ([`error_of`]).
static ERRORED: Mutex<Option<(u64, Instant)>> = Mutex::new(None);
static SENDER: OnceLock<SyncSender<Data>> = OnceLock::new();

thread_local! {
    /// Set on the thread that opened the live encoder. The saver's one-off
    /// still encoder shares `Enc::drain` but never calls `on_encoder_open`,
    /// so its packets are not tapped.
    static LIVE_ENCODER: Cell<bool> = const { Cell::new(false) };
}

struct Stream {
    generation: u64,
    params: ff::codec::Parameters,
    cause: Cause,
}

/// What an encoder's stream looks like to a file: codec, size, input pixel format and
/// parameter sets (SPS/PPS extradata), as `encoder::mux::OwnedParams::same_stream` compares.
#[derive(Debug, Clone, PartialEq)]
pub(super) struct Shape {
    pub codec: i32,
    pub width: i32,
    pub height: i32,
    pub format: i32,
    pub headers: Vec<u8>,
}

fn shape_of(params: &ff::codec::Parameters) -> Shape {
    // SAFETY: `params` owns a valid AVCodecParameters for its whole life; the
    // extradata pointer is valid for `extradata_size` bytes (or null with size 0).
    unsafe {
        let p = &*params.as_ptr();
        let headers = match p.extradata.is_null() || p.extradata_size <= 0 {
            true => Vec::new(),
            false => std::slice::from_raw_parts(p.extradata, p.extradata_size as usize).to_vec(),
        };
        Shape { codec: p.codec_id as i32, width: p.width, height: p.height, format: p.format, headers }
    }
}

/// Why an encoder of shape `new` replaced one of shape `old`. Matching codec, size and
/// headers share one file (`Same`) whatever the input format.
pub(super) fn cause_between(old: Option<&Shape>, new: &Shape) -> Cause {
    match old {
        Some(o) if (o.codec, o.width, o.height) == (new.codec, new.width, new.height) && o.headers == new.headers => Cause::Same,
        Some(o) if (o.width, o.height) != (new.width, new.height) => Cause::Size,
        // D3D11 frames vs RGB0 copies: the encoder's input format.
        Some(o) if o.format != new.format => Cause::Input,
        _ => Cause::Encoder,
    }
}

/// Live encoder (re)opened: keep its codec parameters for the next recording
/// and tell a running one that the stream changed.
pub fn on_encoder_open(encoder: &ff::encoder::Video) {
    LIVE_ENCODER.with(|live| live.set(true));
    let params = ff::codec::Parameters::from(encoder);
    let generation = {
        let mut streams = STREAMS.lock().unwrap_or_else(|p| p.into_inner());
        let cause = cause_between(streams.back().map(|s| shape_of(&s.params)).as_ref(), &shape_of(&params));
        let generation = GENERATION.fetch_add(1, Relaxed) + 1;
        streams.push_back(Stream { generation, params, cause });
        if streams.len() > KEPT_STREAMS {
            streams.pop_front();
        }
        generation
    };
    if ACTIVE.load(Relaxed) {
        if let Some(tx) = SENDER.get() {
            let _ = tx.try_send(Data::EncoderReopened { generation, at: Instant::now() });
        }
    }
}

/// Encoder recovery is spent (`encoder/reopen.rs` hand-off: status `failed`; a single
/// encode error rebuilds in place and does not come here): a running recording of the
/// current generation's stream ends (`recording_failed`, files kept). Repeat calls for
/// the same generation keep the first reason.
pub fn on_encoder_failed(reason: &str) {
    on_encoder_failed_at(GENERATION.load(Relaxed), reason);
}

/// Tests that set a failure hold this (the failure slot is global).
#[cfg(test)]
pub(super) static FAILED_TEST_LOCK: Mutex<()> = Mutex::new(());

pub(super) fn on_encoder_failed_at(generation: u64, reason: &str) {
    let mut failed = FAILED.lock().unwrap_or_else(|p| p.into_inner());
    if failed.as_ref().is_none_or(|(g, _)| *g != generation) {
        *failed = Some((generation, reason.to_string()));
    }
}

/// The live encoder hit an encode error now (`encoder::live`; the recovery reopens it).
pub fn on_encode_error() {
    on_encode_error_at(GENERATION.load(Relaxed), Instant::now());
}

pub(super) fn on_encode_error_at(generation: u64, at: Instant) {
    let mut errored = ERRORED.lock().unwrap_or_else(|p| p.into_inner());
    if errored.is_none_or(|(g, _)| g != generation) {
        *errored = Some((generation, at));
    }
}

/// When live encoder `generation` had its encode error, if it did.
pub(super) fn error_of(generation: u64) -> Option<Instant> {
    ERRORED.lock().unwrap_or_else(|p| p.into_inner()).filter(|(g, _)| *g == generation).map(|(_, at)| at)
}

/// The generation the failure slot names, if any (tests of the encoder loop).
#[cfg(test)]
pub(crate) fn failed_generation() -> Option<u64> {
    FAILED.lock().unwrap_or_else(|p| p.into_inner()).as_ref().map(|(g, _)| *g)
}

#[cfg(test)]
pub(crate) fn generation_now() -> u64 {
    GENERATION.load(Relaxed)
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

/// Live encoder `generation`'s parameters and why it opened (None once it is long gone).
pub(super) fn stream_of(generation: u64) -> Option<(ff::codec::Parameters, Cause)> {
    let streams = STREAMS.lock().unwrap_or_else(|p| p.into_inner());
    streams.iter().find(|s| s.generation == generation).map(|s| (s.params.clone(), s.cause))
}

/// The newest generation and its parameters, read together (None before an encoder opened).
pub(super) fn newest() -> (u64, Option<ff::codec::Parameters>) {
    let streams = STREAMS.lock().unwrap_or_else(|p| p.into_inner());
    let generation = GENERATION.load(Relaxed);
    (generation, streams.back().filter(|s| s.generation == generation).map(|s| s.params.clone()))
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
    use super::{cause_between, tap_part, Cause, Shape, TAP_MAX_SAMPLES};

    const D3D11: i32 = 174;
    const RGB0: i32 = 121;

    fn shape(width: i32, height: i32, format: i32, headers: &[u8]) -> Shape {
        Shape { codec: 27, width, height, format, headers: headers.to_vec() }
    }

    /// Merge gate 11-07 (P2): an input change said "the game's size changed".
    #[test]
    fn a_reopen_names_what_changed() {
        let gpu = shape(1920, 1080, D3D11, b"sps1");
        assert_eq!(cause_between(Some(&gpu), &shape(1280, 720, D3D11, b"sps2")), Cause::Size);
        assert_eq!(cause_between(Some(&gpu), &shape(1920, 1080, RGB0, b"sps3")), Cause::Input);
        assert_eq!(cause_between(Some(&gpu), &shape(1280, 720, RGB0, b"sps2")), Cause::Size, "size first");
        assert_eq!(cause_between(Some(&shape(1920, 1080, RGB0, b"a")), &shape(1920, 1080, RGB0, b"b")), Cause::Encoder);
        assert_eq!(cause_between(None, &gpu), Cause::Encoder);
    }

    /// Merge gate 11-07 (P2): a GPU -> CPU switch with the same parameter sets needs no new file.
    #[test]
    fn a_reopen_with_matching_headers_is_the_same_stream() {
        let gpu = shape(1920, 1080, D3D11, b"sps+pps");
        assert_eq!(cause_between(Some(&gpu), &shape(1920, 1080, RGB0, b"sps+pps")), Cause::Same, "input format is not part of the stream");
        assert_eq!(cause_between(Some(&gpu), &gpu.clone()), Cause::Same);
        let mut hevc = gpu.clone();
        hevc.codec = 173;
        assert_ne!(cause_between(Some(&gpu), &hevc), Cause::Same, "another codec");
    }

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
