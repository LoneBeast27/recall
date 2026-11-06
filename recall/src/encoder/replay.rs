//! The replay buffer, kept apart from the encoder that fills it so it outlives one:
//! a fatal encode error rebuilds the encoder in place and the buffer stays saveable,
//! through the gap and after (`reopen.rs`). Only a real resolution change starts it over.
//!
//! Each encoder that writes into the ring is a segment with its own codec parameters.
//! A save muxes the newest run of segments whose streams are the same
//! (`OwnedParams::same_stream`: codec, size, pixel format, SPS/PPS): NVENC reopened with
//! the same settings gives the same parameter sets, so the whole buffer survives. A run
//! that would need a second sample entry is cut at the newest compatible segment, which
//! starts at that encoder's first packet (a keyframe).

use std::sync::atomic::Ordering::Relaxed;
use std::sync::Arc;

use super::mux::OwnedParams;
use super::saver::RawFrame;
use super::settings::{Codec, Quality};
use super::{Config, EncStats, Enc};
use crate::ring::{EncodedPacket, PacketRing};
use crate::spill::Spill;

/// One encoder's stretch of the ring.
#[derive(Clone)]
pub(super) struct Segment {
    /// Its first packet's PTS (`None` until that packet comes).
    start: Option<u64>,
    pub params: OwnedParams,
    pub backend: &'static str,
    pub codec: Codec,
    width: u32,
    height: u32,
}

impl Segment {
    fn joins(&self, next: &Segment) -> bool {
        self.backend == next.backend && self.width == next.width && self.height == next.height && self.params.same_stream(&next.params)
    }
}

pub(super) struct Replay {
    pub ring: PacketRing,
    segments: Vec<Segment>,
    pub nominal_fps: u32,
    pub bitrate: usize,
    pub quality: Quality,
    /// Newest raw frame (moved in after encode) — thumbnail and still source.
    pub last_frame: Option<RawFrame>,
    /// Newest GPU frame: read back to a `RawFrame` only when a save needs one.
    pub last_gpu: Option<crate::gpu_frames::HwFrame>,
    /// The newest frame's picture region on the canvas (`PacedFrame::content`).
    pub last_content: Option<crate::framing::Rect>,
    pub last_input_pts: Option<u64>,
    /// The "RAM full, dropping old seconds" line was logged for this ring.
    pub dropped_noted: bool,
}

impl Replay {
    pub fn new(cfg: &Config, spill: Option<Arc<Spill>>) -> Self {
        Self {
            ring: PacketRing::with_spill(cfg.buffer_secs as u64 * 1_000_000, spill),
            segments: Vec::new(),
            nominal_fps: cfg.fps,
            bitrate: cfg.bitrate,
            quality: cfg.quality,
            last_frame: None,
            last_gpu: None,
            last_content: None,
            last_input_pts: None,
            dropped_noted: false,
        }
    }

    /// A new encoder writes from its first packet on.
    pub fn begin(&mut self, enc: &Enc) {
        self.begin_segment(Segment { start: None, params: enc.params.clone(), backend: enc.backend, codec: enc.codec, width: enc.width, height: enc.height });
    }

    fn begin_segment(&mut self, seg: Segment) {
        // An encoder that never wrote a packet leaves nothing to keep.
        self.segments.retain(|s| s.start.is_some());
        self.segments.push(seg);
    }

    pub fn push(&mut self, pkt: EncodedPacket) {
        if let Some(seg) = self.segments.last_mut().filter(|s| s.start.is_none()) {
            seg.start = Some(pkt.frame_idx);
        }
        self.ring.push(pkt);
        // Segments the ring has evicted entirely.
        let first = self.ring.first_pts_us().unwrap_or(0);
        while self.segments.len() > 1 && self.segments[1].start.is_some_and(|s| s <= first) {
            self.segments.remove(0);
        }
    }

    /// The newest encoder's (even) frame size.
    pub fn size(&self) -> Option<(u32, u32)> {
        self.segments.last().map(|s| (s.width, s.height))
    }

    pub fn ring_capacity_secs(&self) -> u64 {
        self.ring.capacity_us() / 1_000_000
    }

    /// The newest segment with packets, and where its compatible run starts.
    pub fn run(&self) -> Option<(u64, &Segment)> {
        let newest = self.segments.iter().rposition(|s| s.start.is_some())?;
        let mut first = newest;
        while first > 0 && self.segments[first - 1].start.is_some() && self.segments[first - 1].joins(&self.segments[first]) {
            first -= 1;
        }
        Some((self.segments[first].start.unwrap_or(0), &self.segments[newest]))
    }

    /// The save window: packets of the newest compatible run, and that run's stream.
    pub fn window(&self, want_us: u64, end_us: u64) -> Option<(Vec<EncodedPacket>, &Segment)> {
        let (since, seg) = self.run()?;
        Some((self.ring.save_window_since(want_us, end_us, since), seg))
    }

    /// Buffer counters for status.
    pub fn publish(&self, stats: &EncStats) {
        stats.buffered_frames.store(self.ring.span_frames(), Relaxed);
        stats.buffered_us.store(self.ring.span_us(), Relaxed);
        stats.buffer_start_us.store(self.ring.first_pts_us().unwrap_or(0), Relaxed);
        stats.buffered_bytes.store(self.ring.bytes() as u64, Relaxed);
    }
}

#[cfg(test)]
#[path = "replay_tests.rs"]
mod tests;
