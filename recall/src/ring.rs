//! Compressed-packet replay ring, bounded by elapsed capture time.
//!
//! `frame_idx` deliberately retains its historic field name for callers, but
//! is now a microsecond PTS in the video 1/1_000_000 time base. Packets are
//! copied out with rebased PTS while retaining their VFR durations.

use std::collections::VecDeque;

#[derive(Clone)]
pub struct EncodedPacket {
    /// Capture PTS in microseconds.
    pub frame_idx: u64,
    /// Packet duration in microseconds.
    pub duration_us: u64,
    pub keyframe: bool,
    pub data: Vec<u8>,
}

/// A keyframe-aligned packet ring spanning approximately `capacity_us`.
pub struct PacketRing {
    packets: VecDeque<EncodedPacket>,
    capacity_us: u64,
    bytes: usize,
}

impl PacketRing {
    pub fn new(capacity_us: u64) -> Self {
        Self {
            packets: VecDeque::new(),
            capacity_us,
            bytes: 0,
        }
    }

    pub fn bytes(&self) -> usize {
        self.bytes
    }

    pub fn last_pts_us(&self) -> Option<u64> {
        self.packets.back().map(|p| p.frame_idx)
    }

    /// PTS of the oldest decodable packet retained in the ring.
    pub fn first_pts_us(&self) -> Option<u64> {
        self.packets.front().map(|p| p.frame_idx)
    }

    /// Number of actual encoded packets, not a timestamp-derived frame count.
    pub fn span_frames(&self) -> u64 {
        self.packets.len() as u64
    }

    /// Buffered elapsed time, including the duration of the newest packet.
    pub fn span_us(&self) -> u64 {
        match (self.packets.front(), self.packets.back()) {
            (Some(first), Some(last)) => last
                .frame_idx
                .saturating_add(last.duration_us)
                .saturating_sub(first.frame_idx),
            _ => 0,
        }
    }

    /// Refine the newest packet when the next PTS arrives. This gives the
    /// preceding packet its true capture interval without guessing from fps.
    pub fn set_last_duration_us(&mut self, duration_us: u64) {
        if let Some(last) = self.packets.back_mut() {
            last.duration_us = duration_us.max(1);
        }
    }

    /// Push a packet and evict complete leading GOPs once elapsed time exceeds
    /// capacity. A save can consequently always begin with a decodable frame.
    pub fn push(&mut self, pkt: EncodedPacket) {
        self.bytes += pkt.data.len();
        self.packets.push_back(pkt);
        while self.span_us() > self.capacity_us {
            let Some(second_keyframe) = self
                .packets
                .iter()
                .enumerate()
                .skip(1)
                .find(|(_, packet)| packet.keyframe)
                .map(|(i, _)| i)
            else {
                break;
            };
            for _ in 0..second_keyframe {
                if let Some(packet) = self.packets.pop_front() {
                    self.bytes -= packet.data.len();
                }
            }
        }
    }

    /// Return the last `want_us` of packets, expanded backwards to a keyframe.
    /// Returned PTS values are rebased for a standalone container.
    pub fn save_window(&self, want_us: u64) -> Vec<EncodedPacket> {
        let end = self
            .packets
            .back()
            .map(|p| p.frame_idx.saturating_add(p.duration_us))
            .unwrap_or(0);
        self.save_window_at(want_us, end)
    }

    /// As `save_window`, with an explicit capture-clock end point. This keeps
    /// the newest still frame visible through a quiet tail before saving.
    pub fn save_window_at(&self, want_us: u64, end_us: u64) -> Vec<EncodedPacket> {
        let Some(back) = self.packets.back() else {
            return Vec::new();
        };
        let end = end_us.max(back.frame_idx.saturating_add(back.duration_us));
        let target_start = end.saturating_sub(want_us);
        let start_pos = self
            .packets
            .iter()
            .enumerate()
            .filter(|(_, p)| p.keyframe && p.frame_idx <= target_start)
            .map(|(i, _)| i)
            .next_back()
            .or_else(|| {
                self.packets
                    .iter()
                    .enumerate()
                    .find(|(_, p)| p.keyframe)
                    .map(|(i, _)| i)
            });
        let Some(start_pos) = start_pos else {
            return Vec::new();
        };
        let base = self.packets[start_pos].frame_idx;
        let last_pos = self.packets.len() - 1;
        self.packets
            .iter()
            .enumerate()
            .skip(start_pos)
            .map(|(pos, p)| EncodedPacket {
                frame_idx: p.frame_idx.saturating_sub(base),
                duration_us: if pos == last_pos {
                    let held = end.saturating_sub(p.frame_idx).max(1);
                    // A lone current keyframe may have been held for minutes. The
                    // requested save window is then the desired still duration.
                    if start_pos == last_pos {
                        held.min(want_us.max(1))
                    } else {
                        held
                    }
                } else {
                    p.duration_us
                },
                keyframe: p.keyframe,
                data: p.data.clone(),
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn packet(pts: u64, duration_us: u64, keyframe: bool) -> EncodedPacket {
        EncodedPacket {
            frame_idx: pts,
            duration_us,
            keyframe,
            data: vec![0; 4],
        }
    }

    #[test]
    fn irregular_pts_are_rebased_with_durations() {
        let mut ring = PacketRing::new(10_000_000);
        ring.push(packet(0, 16_000, true));
        ring.push(packet(16_000, 52_000, false));
        ring.push(packet(68_000, 33_000, false));
        let saved = ring.save_window(100_000);
        assert_eq!(
            saved.iter().map(|p| p.frame_idx).collect::<Vec<_>>(),
            vec![0, 16_000, 68_000]
        );
        assert_eq!(
            saved.iter().map(|p| p.duration_us).collect::<Vec<_>>(),
            vec![16_000, 52_000, 33_000]
        );
        assert_eq!(ring.span_us(), 101_000);
        assert_eq!(ring.span_frames(), 3);
    }

    #[test]
    fn save_backs_up_to_keyframe_by_elapsed_time() {
        let mut ring = PacketRing::new(10_000_000);
        ring.push(packet(0, 40_000, true));
        ring.push(packet(40_000, 40_000, false));
        ring.push(packet(80_000, 40_000, true));
        ring.push(packet(120_000, 80_000, false));
        let saved = ring.save_window(90_000);
        assert_eq!(saved[0].frame_idx, 0);
        assert!(saved[0].keyframe);
        assert_eq!(saved.len(), 2);
    }

    #[test]
    fn capacity_evicts_whole_old_gop() {
        let mut ring = PacketRing::new(100_000);
        ring.push(packet(0, 40_000, true));
        ring.push(packet(40_000, 40_000, false));
        ring.push(packet(80_000, 40_000, true));
        ring.push(packet(120_000, 40_000, false));
        assert_eq!(ring.span_frames(), 2);
        assert_eq!(ring.save_window(999_999)[0].frame_idx, 0);
    }

    #[test]
    fn static_tail_is_held_for_requested_window() {
        let mut ring = PacketRing::new(10_000_000);
        ring.push(packet(0, 16_000, true));
        let saved = ring.save_window_at(60_000_000, 300_000_000);
        assert_eq!(saved.len(), 1);
        assert_eq!(saved[0].duration_us, 60_000_000);
    }

    #[test]
    fn multi_packet_static_tail_needs_a_fresh_still() {
        let mut ring = PacketRing::new(10_000_000);
        ring.push(packet(0, 1_000_000, true));
        ring.push(packet(1_000_000, 16_000, false));
        let saved = ring.save_window_at(60_000_000, 300_000_000);
        // The ring cannot safely trim the inter-frame packet. Encoder save
        // detects this shape and makes a fresh independent still keyframe.
        assert_eq!(saved.len(), 2);
        assert_eq!(saved.last().unwrap().duration_us, 299_000_000);
        assert_eq!(ring.first_pts_us(), Some(0));
    }
}
