//! Video keyframe times from a packet scan (no decoding), on the editor
//! timeline (seconds after the source's timeline origin). The lossless export
//! snaps cuts to exactly these instants.

use std::path::Path;

use ff::Rescale;
use ffmpeg_next as ff;

use super::timeline::timeline_origin_us;

pub(crate) struct KeyframeScan {
    video_index: usize,
    time_base: ff::Rational,
    origin_us: i64,
    times_us: Vec<i64>,
}

impl KeyframeScan {
    pub fn for_input(input: &ff::format::context::Input) -> Option<Self> {
        let video = input.streams().best(ff::media::Type::Video)?;
        Some(Self {
            video_index: video.index(),
            time_base: video.time_base(),
            origin_us: timeline_origin_us(input, video.index()),
            times_us: Vec::new(),
        })
    }

    /// Feed every demuxed packet; non-video and non-key packets are ignored.
    pub fn observe(&mut self, stream_index: usize, packet: &ff::Packet) {
        if stream_index != self.video_index || !packet.is_key() {
            return;
        }
        if let Some(pts) = packet.pts().or(packet.dts()) {
            let us = pts
                .rescale(self.time_base, ff::rescale::TIME_BASE)
                .saturating_sub(self.origin_us)
                .max(0);
            self.times_us.push(us);
        }
    }

    /// Ascending, de-duplicated keyframe times in seconds (µs precision).
    pub fn seconds(mut self) -> Vec<f64> {
        self.times_us.sort_unstable();
        self.times_us.dedup();
        self.times_us.into_iter().map(|us| us as f64 / 1_000_000.0).collect()
    }
}

/// A dedicated demux pass over `source` (used when no audio pass can carry it).
pub(crate) fn scan_keyframes(source: &Path) -> Result<Vec<f64>, String> {
    let mut input = ff::format::input(source)
        .map_err(|e| format!("open {} for keyframes: {e}", source.display()))?;
    let mut scan = KeyframeScan::for_input(&input).ok_or("input has no video stream")?;
    for (stream, packet) in input.packets() {
        scan.observe(stream.index(), &packet);
    }
    Ok(scan.seconds())
}
