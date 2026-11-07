//! The long-recording state machine: `Idle -> Armed -> Recording -> Idle`,
//! driven by the writer thread and testable with an injected [`Sink`].
//!
//! - **Armed** waits for the first video KEYFRAME (a fresh file must start
//!   decodable; the GOP is 1 s). No keyframe within [`ARM_TIMEOUT`] fails.
//! - **Recording** writes each packet one step late, so its duration is the
//!   next packet's PTS minus its own (true VFR). Dropped packets (queue full)
//!   resync at the next keyframe. Audio is placed by wall clock: each chunk's
//!   position is `(arrival - wall_base) * rate - len`; gaps over
//!   [`AUDIO_TOLERANCE_MS`] are padded with silence, overruns are trimmed, and
//!   a tick pads silent tracks (loopback delivers nothing while nothing plays)
//!   so every track keeps pace with the video.
//! - **Stop** flushes the held packet, finalizes the sink, renames `.part` to
//!   the final name and writes the marks sidecar. A failing sink is abandoned
//!   and closed: the partial file is still renamed into place (a fragmented
//!   MP4 plays up to its last complete fragment) and `Failed{kept}` names it.
//! - **Encoder reopened** (capture size or frame path changed): the part ends
//!   and the recording goes on in the next part (`machine_parts.rs`).
//! - **Encoder failed**: the stream ends with its encoder, reported as
//!   `Failed{kept}` naming the finished file.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use super::TrackInfo;
use crate::export::mix_limit::stream::StreamLimiter;
use crate::naming;

/// How long `Armed` waits for a keyframe.
pub const ARM_TIMEOUT: Duration = Duration::from_secs(30);
/// Audio drift tolerated before padding / trimming.
pub const AUDIO_TOLERANCE_MS: u64 = 100;
/// Silent tracks are padded up to this far behind real time.
pub const PAD_LAG_MS: u64 = 250;
/// Duration given to the very last packet.
const TAIL_US: u64 = 16_667;

/// Where a recording is written (the real one is `fmp4::Fmp4Sink`).
pub trait Sink {
    fn video(&mut self, pts_us: u64, duration_us: u64, keyframe: bool, data: &[u8]) -> Result<(), String>;
    /// Interleaved f32 for `track`, contiguous with what came before.
    fn audio(&mut self, track: usize, samples: &[f32]) -> Result<(), String>;
    /// `frames` of silence for `track`.
    fn silence(&mut self, track: usize, frames: u64) -> Result<(), String>;
    /// Trailer + flush + close. Returns the file size in bytes.
    fn finish(&mut self) -> Result<u64, String>;
    /// Close without a trailer, keeping only complete fragments.
    fn abandon(&mut self);
}

pub trait SinkFactory {
    type Sink: Sink;
    /// The video stream's settings a part opens with (the encoder's codec parameters).
    type Video: Clone;
    /// `video`: the settings of the encoder whose packets the part gets (None before one opened).
    fn open(&mut self, part: &Path, tracks: &[TrackInfo], video: Option<&Self::Video>) -> Result<Self::Sink, String>;
    /// The settings of live encoder `generation` and why it (re)opened.
    fn stream_of(&self, generation: u64) -> Option<(Self::Video, Cause)>;
    /// The newest live encoder generation and its settings, read together.
    fn newest(&self) -> (u64, Option<Self::Video>);
}

#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    Started { path: PathBuf },
    /// The recording ended. `seconds`: this file; `total_seconds`: every part; `parts`: files written;
    /// `gap_seconds`: time not recorded between the parts; `frozen_seconds`: time the picture
    /// held still after a same-header encoder reopen, waiting for its keyframe.
    Stopped { path: PathBuf, seconds: f64, bytes: u64, marks: usize, parts: u32, total_seconds: f64, gap_seconds: f64, frozen_seconds: f64 },
    /// Part `part` ended because the stream changed (`cause`); the recording goes on.
    Continued { path: PathBuf, seconds: f64, bytes: u64, marks: usize, part: u32, total_seconds: f64, cause: Cause },
    /// Part `part` (2, 3, ...) of a continued recording is writing.
    PartStarted { path: PathBuf, part: u32, cause: Cause },
    Failed { reason: String, kept: Option<PathBuf> },
    Log(String),
}

struct Lane {
    rate: u64,
    channels: usize,
    /// Frames placed on the track so far (its limiter may still hold the newest ~10 ms).
    written: u64,
    /// Every frame passes through the limiter in order, from the track's first frame (SPEC A3:
    /// the limiter always runs), so a track that first goes above the -1 dBFS ceiling mid-stream
    /// (a source boosted live, a game transient) is ramped down ahead of its peak instead of
    /// stepping in one sample. Under the ceiling its output is bit-identical to the input; a quiet
    /// limiter lets silence take the sink's fast path (`write_silence`).
    limiter: StreamLimiter,
}

struct Active<S> {
    sink: S,
    path: PathBuf,
    part: PathBuf,
    base_us: u64,
    wall_base: Instant,
    held: Option<(u64, bool, Arc<[u8]>)>,
    last_pts: Option<u64>,
    last_duration: u64,
    need_key: bool,
    video_packets: u64,
    lanes: Vec<Lane>,
    marks: Vec<u64>,
    /// This part's number (1 = the first file) and the seconds of the parts before it.
    number: u32,
    earlier_secs: f64,
    /// Seconds not recorded between the parts before this one and it.
    earlier_gap: f64,
    /// When this part's first keyframe was written.
    began: Instant,
    /// Seconds frozen in the parts before this one; µs frozen in this one; the capture µs of
    /// the last packet before a same-header reopen still waiting for its keyframe.
    earlier_frozen: f64,
    frozen_us: u64,
    freeze_from: Option<u64>,
    /// The capture µs of the live encoder's encode error, until its reopen is seen.
    error_us: Option<u64>,
}

enum Phase<S> {
    Idle,
    /// `start`: (capture-clock µs, the `Instant` it was read at) of the request.
    /// `previous`: the part this one continues (None for a fresh recording).
    Armed { path: PathBuf, start: Option<(u64, Instant)>, requested: Instant, previous: Option<Series> },
    Recording(Box<Active<S>>),
}

pub struct Recorder<F: SinkFactory> {
    factory: F,
    tracks: Vec<TrackInfo>,
    phase: Phase<F::Sink>,
    /// The settings of the encoder whose packets the next part gets.
    video: Option<F::Video>,
}

impl<F: SinkFactory> Recorder<F> {
    pub fn new(factory: F, tracks: Vec<TrackInfo>) -> Self {
        Recorder { factory, tracks, phase: Phase::Idle, video: None }
    }

    pub fn factory(&self) -> &F {
        &self.factory
    }

    /// The encoder settings the next part opens with (snapshotted with its generation).
    pub fn set_video(&mut self, video: Option<F::Video>) {
        self.video = video;
    }

    /// Armed or recording.
    pub fn is_active(&self) -> bool {
        !matches!(self.phase, Phase::Idle)
    }

    pub fn track_index(&self, name: &str) -> Option<usize> {
        self.tracks.iter().position(|t| t.name == name)
    }

    /// Arm a recording to `path` (its `.part` is written). `start` is the
    /// capture clock at the request and the `Instant` it was sampled at (None
    /// before the first frame); `now` starts the keyframe wait.
    pub fn start(&mut self, path: PathBuf, start: Option<(u64, Instant)>, now: Instant) -> Vec<Event> {
        if self.is_active() {
            return vec![Event::Log("record_start ignored: a long recording is already running".into())];
        }
        self.phase = Phase::Armed { path, start, requested: now, previous: None };
        Vec::new()
    }

    pub fn stop(&mut self) -> Vec<Event> {
        match std::mem::replace(&mut self.phase, Phase::Idle) {
            Phase::Idle => vec![Event::Log("record_stop ignored: no long recording is running".into())],
            // Stopped between two parts: the recording is the finished part.
            Phase::Armed { previous: Some(p), .. } => vec![p.stopped()],
            Phase::Armed { .. } => vec![Event::Failed { reason: "stopped before the first keyframe arrived".into(), kept: None }],
            Phase::Recording(active) => finish(*active),
        }
    }

    pub fn mark(&mut self, at_us: u64) {
        if let Phase::Recording(a) = &mut self.phase {
            a.marks.push(at_us);
        }
    }

    pub fn video(&mut self, pts_us: u64, keyframe: bool, data: Arc<[u8]>, gap: bool, now: Instant) -> Vec<Event> {
        match &mut self.phase {
            Phase::Idle => Vec::new(),
            Phase::Armed { .. } if !keyframe => Vec::new(),
            Phase::Armed { previous: Some(s), .. } if now < s.not_before => Vec::new(),
            Phase::Armed { .. } => self.begin(pts_us, data, now),
            Phase::Recording(a) => match a.push_video(pts_us, keyframe, data, gap) {
                Ok(()) => Vec::new(),
                Err(e) => self.fail(e),
            },
        }
    }

    pub fn audio(&mut self, track: usize, samples: &[f32], at: Instant) -> Vec<Event> {
        let Phase::Recording(a) = &mut self.phase else { return Vec::new() };
        match a.push_audio(track, samples, at) {
            Ok(()) => Vec::new(),
            Err(e) => self.fail(e),
        }
    }

    /// Arm timeout + silence padding for tracks that delivered nothing.
    pub fn tick(&mut self, now: Instant) -> Vec<Event> {
        match &mut self.phase {
            Phase::Armed { requested, previous, .. } if now.saturating_duration_since(arm_wait_from(*requested, previous)) >= ARM_TIMEOUT => {
                let kept = previous.as_ref().map(|p| p.path.clone());
                self.phase = Phase::Idle;
                vec![Event::Failed { reason: "no video keyframe arrived; is the game rendering?".into(), kept }]
            }
            Phase::Recording(a) => match a.pad_silent(now) {
                Ok(()) => Vec::new(),
                Err(e) => self.fail(e),
            },
            _ => Vec::new(),
        }
    }

    fn begin(&mut self, pts_us: u64, data: Arc<[u8]>, now: Instant) -> Vec<Event> {
        let Phase::Armed { path, start, previous, .. } = std::mem::replace(&mut self.phase, Phase::Idle) else {
            return Vec::new();
        };
        // The capture clock runs in real time: map this keyframe onto the
        // wall clock through the (capture time, Instant) pair of the request.
        let wall_base = match start {
            Some((s, at)) if pts_us >= s => at + Duration::from_micros(pts_us - s),
            Some((s, at)) => at.checked_sub(Duration::from_micros(s - pts_us)).unwrap_or(at),
            None => now,
        };
        let part = naming::part_of(&path);
        let sink = match self.factory.open(&part, &self.tracks, self.video.as_ref()) {
            Ok(s) => s,
            Err(reason) => {
                let _ = std::fs::remove_file(&part);
                return vec![Event::Failed { reason, kept: previous.map(|p| p.path) }];
            }
        };
        let lanes = self
            .tracks
            .iter()
            .map(|t| Lane {
                rate: t.rate as u64,
                channels: t.channels.max(1) as usize,
                written: 0,
                limiter: StreamLimiter::new(t.channels, t.rate),
            })
            .collect();
        let mut active = Box::new(Active {
            sink,
            path: path.clone(),
            part,
            base_us: pts_us,
            wall_base,
            held: None,
            last_pts: None,
            last_duration: TAIL_US,
            need_key: false,
            video_packets: 0,
            lanes,
            marks: Vec::new(),
            number: previous.as_ref().map_or(1, |p| p.part + 1),
            earlier_secs: previous.as_ref().map_or(0.0, |p| p.total_seconds),
            earlier_gap: previous.as_ref().map_or(0.0, |p| p.gap_seconds + p.end_us.map_or(0.0, |end| pts_us.saturating_sub(end) as f64 / 1e6)),
            began: now,
            earlier_frozen: previous.as_ref().map_or(0.0, |p| p.frozen_seconds),
            frozen_us: 0,
            freeze_from: None,
            error_us: None,
        });
        if let Err(e) = active.push_video(pts_us, true, data, false) {
            self.phase = Phase::Recording(active);
            return self.fail(e);
        }
        let started = match previous {
            Some(p) => Event::PartStarted { path, part: active.number, cause: p.cause },
            None => Event::Started { path },
        };
        self.phase = Phase::Recording(active);
        vec![started]
    }

    fn fail(&mut self, reason: String) -> Vec<Event> {
        let Phase::Recording(active) = std::mem::replace(&mut self.phase, Phase::Idle) else {
            return vec![Event::Failed { reason, kept: None }];
        };
        let Active { mut sink, part, video_packets, .. } = *active;
        sink.abandon();
        drop(sink);
        let kept = if video_packets > 0 {
            keep_partial(&part)
        } else {
            let _ = std::fs::remove_file(&part);
            None
        };
        vec![Event::Failed { reason, kept }]
    }
}

impl<S: Sink> Active<S> {
    fn push_video(&mut self, pts_us: u64, keyframe: bool, data: Arc<[u8]>, gap: bool) -> Result<(), String> {
        if gap {
            self.need_key = true;
        }
        if self.need_key && !keyframe {
            return Ok(());
        }
        let newest = self.held.as_ref().map(|h| h.0).or(self.last_pts);
        if pts_us < self.base_us || newest.is_some_and(|p| pts_us <= p) {
            return Ok(()); // non-monotonic: never hand the muxer a regression
        }
        self.need_key = false;
        // A same-header reopen's freeze ends at the new encoder's first keyframe.
        if let Some(from) = self.freeze_from.take_if(|_| keyframe) {
            self.frozen_us += pts_us.saturating_sub(from);
        }
        if let Some((pts, key, payload)) = self.held.take() {
            let duration = (pts_us - pts).max(1);
            self.write_video(pts, duration, key, &payload)?;
        }
        self.held = Some((pts_us, keyframe, data));
        Ok(())
    }

    fn write_video(&mut self, pts: u64, duration: u64, key: bool, data: &[u8]) -> Result<(), String> {
        self.sink.video(pts - self.base_us, duration, key, data)?;
        self.last_pts = Some(pts);
        self.last_duration = duration;
        self.video_packets += 1;
        Ok(())
    }

    /// Frames of real time elapsed at `at` since the recording's first frame.
    fn wall_frames(&self, rate: u64, at: Instant) -> i64 {
        let us = if at >= self.wall_base {
            at.duration_since(self.wall_base).as_micros() as i64
        } else {
            -(self.wall_base.duration_since(at).as_micros() as i64)
        };
        us * rate as i64 / 1_000_000
    }

    fn push_audio(&mut self, track: usize, samples: &[f32], at: Instant) -> Result<(), String> {
        let Some(lane) = self.lanes.get(track) else { return Ok(()) };
        let (rate, channels, written) = (lane.rate, lane.channels, lane.written as i64);
        let frames = (samples.len() / channels) as i64;
        let end = self.wall_frames(rate, at);
        let tolerance = (rate * AUDIO_TOLERANCE_MS / 1000) as i64;
        let start = end - frames;
        // Frames of this chunk to drop from its front, and silence to put first.
        let mut skip = (-start).max(0);
        let placed = start.max(0);
        let mut pad = 0;
        if placed > written + tolerance {
            pad = placed - written;
        } else if placed + tolerance < written {
            skip = skip.max(written - placed);
        }
        let skip = skip.min(frames) as usize;
        if pad > 0 {
            self.write_silence(track, pad as u64)?;
        }
        let keep = &samples[skip * channels..frames as usize * channels];
        if !keep.is_empty() {
            self.write_audio(track, keep)?;
        }
        self.lanes[track].written = (written + pad) as u64 + (keep.len() / channels) as u64;
        Ok(())
    }

    fn pad_silent(&mut self, now: Instant) -> Result<(), String> {
        for track in 0..self.lanes.len() {
            let rate = self.lanes[track].rate;
            let target = self.wall_frames(rate, now) - (rate * PAD_LAG_MS / 1000) as i64;
            let written = self.lanes[track].written as i64;
            if target - written > (rate * AUDIO_TOLERANCE_MS / 1000) as i64 {
                self.write_silence(track, (target - written) as u64)?;
                self.lanes[track].written = target as u64;
            }
        }
        Ok(())
    }

    /// Samples for `track`, through its limiter (the newest ~10 ms stay held as look-ahead).
    fn write_audio(&mut self, track: usize, samples: &[f32]) -> Result<(), String> {
        let out = self.lanes[track].limiter.push(samples);
        if out.is_empty() { Ok(()) } else { self.sink.audio(track, &out) }
    }

    /// Silence for `track`, behind the frames its limiter still holds. While the limiter is
    /// quiet (nothing near the ceiling, gain back at 1) those frames go out as captured and the
    /// silence takes the sink's fast path; else it passes through the limiter as zeros, until
    /// the limiter is quiet again.
    fn write_silence(&mut self, track: usize, frames: u64) -> Result<(), String> {
        let channels = self.lanes[track].channels;
        let zeros = vec![0.0f32; 4_800 * channels];
        let mut left = frames as usize;
        while left > 0 && !self.lanes[track].limiter.quiet() {
            let n = left.min(4_800);
            self.write_audio(track, &zeros[..n * channels])?;
            left -= n;
        }
        if left == 0 {
            return Ok(());
        }
        let held = self.lanes[track].limiter.drain();
        if !held.is_empty() {
            self.sink.audio(track, &held)?;
        }
        self.sink.silence(track, left as u64)
    }

    /// Stop: each limiter hands on the frames it still holds, so no sample is lost or moved.
    fn flush_limiters(&mut self) -> Result<(), String> {
        for track in 0..self.lanes.len() {
            let out = self.lanes[track].limiter.finish();
            if !out.is_empty() {
                self.sink.audio(track, &out)?;
            }
        }
        Ok(())
    }
}

fn finish<S: Sink>(mut a: Active<S>) -> Vec<Event> {
    if let Some((pts, key, payload)) = a.held.take() {
        let duration = a.last_duration.max(1);
        if let Err(reason) = a.write_video(pts, duration, key, &payload) {
            return vec![Event::Failed { reason, kept: abandon_and_keep(a.sink, &a.part) }];
        }
    }
    if let Err(reason) = a.flush_limiters() {
        return vec![Event::Failed { reason, kept: abandon_and_keep(a.sink, &a.part) }];
    }
    let seconds = a.last_pts.map(|p| (p + a.last_duration - a.base_us) as f64 / 1_000_000.0).unwrap_or(0.0);
    let bytes = match a.sink.finish() {
        Ok(b) => b,
        Err(reason) => return vec![Event::Failed { reason, kept: abandon_and_keep(a.sink, &a.part) }],
    };
    drop(a.sink);
    let path = match std::fs::rename(&a.part, &a.path) {
        Ok(()) => a.path.clone(),
        Err(e) => return vec![Event::Failed { reason: format!("finalize {}: {e}", a.path.display()), kept: None }],
    };
    let rel = crate::bookmarks::window_marks(a.marks.iter().copied(), a.base_us, a.base_us + (seconds * 1e6) as u64);
    let mut events = Vec::new();
    if !rel.is_empty() {
        if let Err(e) = crate::bookmarks::write_sidecar(&path, &rel) {
            events.push(Event::Log(format!("long recording marks not written: {e}")));
        }
    }
    let (parts, total_seconds, gap_seconds) = (a.number, a.earlier_secs + seconds, a.earlier_gap);
    let frozen_seconds = a.earlier_frozen + a.frozen_us as f64 / 1e6;
    events.push(Event::Stopped { path, seconds, bytes, marks: rel.len(), parts, total_seconds, gap_seconds, frozen_seconds });
    events
}

/// Abandon `sink` and CLOSE it (the `.part` cannot be renamed while it is
/// open), then keep the partial recording.
fn abandon_and_keep<S: Sink>(mut sink: S, part: &Path) -> Option<PathBuf> {
    sink.abandon();
    drop(sink);
    keep_partial(part)
}

/// Rename a partial recording into place; `None` when that fails too.
fn keep_partial(part: &Path) -> Option<PathBuf> {
    let target = naming::recovered_path(part, Path::exists);
    std::fs::rename(part, &target).ok().map(|_| target)
}

#[path = "machine_parts.rs"]
mod parts;
pub use parts::Cause;
#[cfg(test)]
pub use parts::PART_GAP;
use parts::{arm_wait_from, Series};

#[cfg(test)]
#[path = "machine_tests.rs"]
mod tests;
