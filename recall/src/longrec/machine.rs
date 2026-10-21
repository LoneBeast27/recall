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
//!   the final name and writes the marks sidecar. A failing sink is abandoned:
//!   the partial file is still renamed into place (a fragmented MP4 plays up to
//!   its last complete fragment) and `Failed{kept}` names it.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use super::TrackInfo;
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
    fn open(&mut self, part: &Path, tracks: &[TrackInfo]) -> Result<Self::Sink, String>;
}

#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    Started { path: PathBuf },
    Stopped { path: PathBuf, seconds: f64, bytes: u64, marks: usize },
    Failed { reason: String, kept: Option<PathBuf> },
    Log(String),
}

struct Lane {
    rate: u64,
    channels: usize,
    /// Frames handed to the sink so far.
    written: u64,
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
}

enum Phase<S> {
    Idle,
    Armed { path: PathBuf, start_us: Option<u64>, requested: Instant },
    Recording(Box<Active<S>>),
}

pub struct Recorder<F: SinkFactory> {
    factory: F,
    tracks: Vec<TrackInfo>,
    phase: Phase<F::Sink>,
}

impl<F: SinkFactory> Recorder<F> {
    pub fn new(factory: F, tracks: Vec<TrackInfo>) -> Self {
        Recorder { factory, tracks, phase: Phase::Idle }
    }

    /// Armed or recording.
    pub fn is_active(&self) -> bool {
        !matches!(self.phase, Phase::Idle)
    }

    pub fn track_index(&self, name: &str) -> Option<usize> {
        self.tracks.iter().position(|t| t.name == name)
    }

    /// Arm a recording to `path` (its `.part` is written). `start_us` is the
    /// capture clock at the request (None before the first frame).
    pub fn start(&mut self, path: PathBuf, start_us: Option<u64>, now: Instant) -> Vec<Event> {
        if self.is_active() {
            return vec![Event::Log("record_start ignored: a long recording is already running".into())];
        }
        self.phase = Phase::Armed { path, start_us, requested: now };
        Vec::new()
    }

    pub fn stop(&mut self) -> Vec<Event> {
        match std::mem::replace(&mut self.phase, Phase::Idle) {
            Phase::Idle => vec![Event::Log("record_stop ignored: no long recording is running".into())],
            Phase::Armed { .. } => vec![Event::Failed { reason: "stopped before the first keyframe arrived".into(), kept: None }],
            Phase::Recording(active) => finish(*active),
        }
    }

    /// The live encoder reopened (resolution change): the stream's parameters
    /// changed, so a running recording ends here.
    pub fn encoder_reopened(&mut self) -> Vec<Event> {
        if !matches!(self.phase, Phase::Recording(_)) {
            return Vec::new();
        }
        let mut events = vec![Event::Log("capture resolution changed; the long recording ends here".into())];
        events.extend(self.stop());
        events
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
            Phase::Armed { requested, .. } if now.saturating_duration_since(*requested) >= ARM_TIMEOUT => {
                self.phase = Phase::Idle;
                vec![Event::Failed { reason: "no video keyframe arrived; is the game rendering?".into(), kept: None }]
            }
            Phase::Recording(a) => match a.pad_silent(now) {
                Ok(()) => Vec::new(),
                Err(e) => self.fail(e),
            },
            _ => Vec::new(),
        }
    }

    fn begin(&mut self, pts_us: u64, data: Arc<[u8]>, now: Instant) -> Vec<Event> {
        let Phase::Armed { path, start_us, requested } = std::mem::replace(&mut self.phase, Phase::Idle) else {
            return Vec::new();
        };
        // The capture clock runs in real time: map this keyframe onto the
        // wall clock through the (capture time, Instant) pair of the request.
        let wall_base = match start_us {
            Some(s) if pts_us >= s => requested + Duration::from_micros(pts_us - s),
            Some(s) => requested.checked_sub(Duration::from_micros(s - pts_us)).unwrap_or(requested),
            None => now,
        };
        let part = naming::part_of(&path);
        let sink = match self.factory.open(&part, &self.tracks) {
            Ok(s) => s,
            Err(reason) => {
                let _ = std::fs::remove_file(&part);
                return vec![Event::Failed { reason, kept: None }];
            }
        };
        let lanes = self.tracks.iter().map(|t| Lane { rate: t.rate as u64, channels: t.channels.max(1) as usize, written: 0 }).collect();
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
        });
        if let Err(e) = active.push_video(pts_us, true, data, false) {
            self.phase = Phase::Recording(active);
            return self.fail(e);
        }
        self.phase = Phase::Recording(active);
        vec![Event::Started { path }]
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
            self.sink.silence(track, pad as u64)?;
        }
        let keep = &samples[skip * channels..frames as usize * channels];
        if !keep.is_empty() {
            self.sink.audio(track, keep)?;
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
                self.sink.silence(track, (target - written) as u64)?;
                self.lanes[track].written = target as u64;
            }
        }
        Ok(())
    }
}

fn finish<S: Sink>(mut a: Active<S>) -> Vec<Event> {
    if let Some((pts, key, payload)) = a.held.take() {
        let duration = a.last_duration.max(1);
        if let Err(reason) = a.write_video(pts, duration, key, &payload) {
            a.sink.abandon();
            return vec![Event::Failed { reason, kept: keep_partial(&a.part) }];
        }
    }
    let seconds = a.last_pts.map(|p| (p + a.last_duration - a.base_us) as f64 / 1_000_000.0).unwrap_or(0.0);
    let bytes = match a.sink.finish() {
        Ok(b) => b,
        Err(reason) => {
            a.sink.abandon();
            return vec![Event::Failed { reason, kept: keep_partial(&a.part) }];
        }
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
    events.push(Event::Stopped { path, seconds, bytes, marks: rel.len() });
    events
}

/// Rename a partial recording into place; `None` when that fails too.
fn keep_partial(part: &Path) -> Option<PathBuf> {
    let target = naming::recovered_path(part, Path::exists);
    std::fs::rename(part, &target).ok().map(|_| target)
}

#[cfg(test)]
#[path = "machine_tests.rs"]
mod tests;
