//! Compressed-packet replay ring: holds the last N seconds of encoded video
//! packets in RAM. The encoder pushes; `save_window` returns a clean packet
//! run for muxing — always starting on a keyframe so the clip decodes from
//! frame zero (the CFR-at-boundary requirement, R1).

use std::collections::VecDeque;

/// One encoded frame out of the video encoder (h264_nvenc), annotated with the
/// CFR frame index it was encoded at (PTS = index / fps, exact by construction).
#[derive(Clone)]
pub struct EncodedPacket {
    /// Monotonic CFR frame index assigned by the pacer.
    pub frame_idx: u64,
    pub keyframe: bool,
    pub data: Vec<u8>,
}

/// Ring of encoded packets spanning at most `capacity_frames` + one GOP.
pub struct PacketRing {
    packets: VecDeque<EncodedPacket>,
    capacity_frames: u64,
    bytes: usize,
}

impl PacketRing {
    pub fn new(capacity_frames: u64) -> Self {
        Self {
            packets: VecDeque::new(),
            capacity_frames,
            bytes: 0,
        }
    }

    /// Current RAM footprint of buffered packet payloads.
    pub fn bytes(&self) -> usize {
        self.bytes
    }

    /// Frames currently spanned (newest - oldest + 1), 0 when empty.
    pub fn span_frames(&self) -> u64 {
        match (self.packets.front(), self.packets.back()) {
            (Some(f), Some(b)) => b.frame_idx - f.frame_idx + 1,
            _ => 0,
        }
    }

    /// Push a packet; evict whole leading GOPs once the span exceeds capacity.
    /// Eviction is GOP-granular (front must stay a keyframe) so a save is
    /// always decodable from its first packet.
    pub fn push(&mut self, pkt: EncodedPacket) {
        self.bytes += pkt.data.len();
        self.packets.push_back(pkt);

        while self.span_frames() > self.capacity_frames {
            // Find the second keyframe; evict everything before it. If there
            // is no second keyframe the single leading GOP is all we have —
            // keep it (capacity is a target, not a hard invariant mid-GOP).
            let Some(second_kf) = self
                .packets
                .iter()
                .enumerate()
                .skip(1)
                .find(|(_, p)| p.keyframe)
                .map(|(i, _)| i)
            else {
                break;
            };
            // Only evict if doing so still leaves >= capacity worth of frames
            // OR we are over budget by more than a GOP (keeps span tight).
            let front_of_rest = self.packets[second_kf].frame_idx;
            let back = self.packets.back().expect("non-empty").frame_idx;
            if back - front_of_rest + 1 >= self.capacity_frames.min(back + 1) {
                for _ in 0..second_kf {
                    if let Some(p) = self.packets.pop_front() {
                        self.bytes -= p.data.len();
                    }
                }
            } else {
                break;
            }
        }
    }

    /// Packets for "save the last `want_frames`": from the newest keyframe at
    /// or before (newest - want_frames + 1) — or the oldest keyframe if the
    /// request exceeds what's buffered — through the newest packet.
    /// Returns a PTS-rebased copy (first packet -> frame 0). Empty when no
    /// keyframe is buffered yet.
    pub fn save_window(&self, want_frames: u64) -> Vec<EncodedPacket> {
        let Some(back) = self.packets.back() else {
            return Vec::new();
        };
        let target_start = back.frame_idx.saturating_sub(want_frames.saturating_sub(1));

        // Newest keyframe with frame_idx <= target_start; else oldest keyframe.
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
        self.packets
            .iter()
            .skip(start_pos)
            .map(|p| EncodedPacket {
                frame_idx: p.frame_idx - base,
                keyframe: p.keyframe,
                data: p.data.clone(),
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kf(idx: u64) -> EncodedPacket {
        EncodedPacket {
            frame_idx: idx,
            keyframe: true,
            data: vec![0u8; 100],
        }
    }
    fn df(idx: u64) -> EncodedPacket {
        EncodedPacket {
            frame_idx: idx,
            keyframe: false,
            data: vec![0u8; 10],
        }
    }

    /// Push `gops` GOPs of `gop` frames each, starting at frame 0.
    fn fill(ring: &mut PacketRing, gops: u64, gop: u64) {
        for g in 0..gops {
            ring.push(kf(g * gop));
            for i in 1..gop {
                ring.push(df(g * gop + i));
            }
        }
    }

    #[test]
    fn save_starts_on_keyframe_and_rebases() {
        let mut ring = PacketRing::new(600);
        fill(&mut ring, 5, 60); // frames 0..299
        let w = ring.save_window(120); // want last 120 -> target_start 180
        assert!(w[0].keyframe);
        assert_eq!(w[0].frame_idx, 0); // rebased
        assert_eq!(w.len(), 120); // kf at 180 exactly
    }

    #[test]
    fn save_clamps_to_buffered_history() {
        let mut ring = PacketRing::new(600);
        fill(&mut ring, 2, 60); // 120 frames buffered
        let w = ring.save_window(100_000);
        assert_eq!(w.len(), 120);
        assert!(w[0].keyframe);
    }

    #[test]
    fn eviction_keeps_front_keyframe_and_capacity() {
        let mut ring = PacketRing::new(120); // 2 GOPs of 60
        fill(&mut ring, 10, 60);
        // Front must be a keyframe and span must be near capacity.
        assert!(ring.packets.front().unwrap().keyframe);
        assert!(ring.span_frames() <= 180); // capacity + <=1 GOP slack
        assert!(ring.span_frames() >= 120);
    }

    #[test]
    fn empty_ring_saves_nothing() {
        let ring = PacketRing::new(600);
        assert!(ring.save_window(60).is_empty());
    }

    #[test]
    fn mid_gop_save_backs_up_to_keyframe() {
        let mut ring = PacketRing::new(600);
        fill(&mut ring, 3, 60); // 0..179
        // Want last 30 -> target_start 150 (mid-GOP) -> start at kf 120.
        let w = ring.save_window(30);
        assert!(w[0].keyframe);
        assert_eq!(w.len(), 60); // from kf 120 through 179
    }
}
