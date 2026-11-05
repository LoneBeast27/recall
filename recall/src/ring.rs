//! Compressed-packet replay ring, bounded by elapsed capture time and by RAM.
//!
//! `frame_idx` deliberately retains its historic field name for callers, but
//! is now a microsecond PTS in the video 1/1_000_000 time base. Save windows
//! copy packet headers out with rebased PTS while retaining their VFR
//! durations; payloads are shared (`Payload`, refcounted), so a window costs a
//! refcount bump per packet instead of a deep copy of the whole replay.
//!
//! With a `Spill` attached, payloads past the RAM budget move to a disk file,
//! oldest first, and the ring keeps its full length (`spill.rs`). Without one,
//! or when spilling fails, the oldest GOPs are dropped instead once RAM passes
//! the budget's hard limit.

use std::collections::VecDeque;
use std::sync::Arc;

pub use crate::spill::Payload;
use crate::spill::Spill;

#[derive(Clone)]
pub struct EncodedPacket {
    /// Capture PTS in microseconds.
    pub frame_idx: u64,
    /// Packet duration in microseconds.
    pub duration_us: u64,
    pub keyframe: bool,
    /// Immutable encoded payload, shared between the ring and save windows.
    pub data: Payload,
}

/// A keyframe-aligned packet ring spanning approximately `capacity_us`.
pub struct PacketRing {
    packets: VecDeque<EncodedPacket>,
    capacity_us: u64,
    bytes: usize,
    spill: Option<Arc<Spill>>,
    /// Packets before this index were already handed to the spill thread.
    spilled_to: usize,
    /// GOPs dropped because RAM ran out and spilling was not possible.
    dropped_for_ram: u64,
}

impl PacketRing {
    #[cfg(test)]
    pub fn new(capacity_us: u64) -> Self {
        Self::with_spill(capacity_us, None)
    }

    /// A ring whose payloads count against `spill`'s RAM budget.
    pub fn with_spill(capacity_us: u64, spill: Option<Arc<Spill>>) -> Self {
        Self {
            packets: VecDeque::new(),
            capacity_us,
            bytes: 0,
            spill,
            spilled_to: 0,
            dropped_for_ram: 0,
        }
    }

    /// Payload bytes held, in RAM and on disk.
    pub fn bytes(&self) -> usize {
        self.bytes
    }

    pub fn capacity_us(&self) -> u64 {
        self.capacity_us
    }

    /// GOPs dropped so far because neither RAM nor the spill file had room.
    pub fn dropped_for_ram(&self) -> u64 {
        self.dropped_for_ram
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

    /// Index of the second keyframe: dropping everything before it keeps the
    /// ring starting on a decodable frame.
    fn second_keyframe(&self) -> Option<usize> {
        self.packets
            .iter()
            .enumerate()
            .skip(1)
            .find(|(_, packet)| packet.keyframe)
            .map(|(i, _)| i)
    }

    /// Drop the oldest GOP. Returns the RAM it held, or `None` when only one GOP is left.
    fn drop_front_gop(&mut self) -> Option<usize> {
        let second_keyframe = self.second_keyframe()?;
        let mut ram = 0;
        for _ in 0..second_keyframe {
            if let Some(packet) = self.packets.pop_front() {
                self.bytes -= packet.data.len();
                if packet.data.in_ram() {
                    ram += packet.data.len();
                }
            }
        }
        self.spilled_to = self.spilled_to.saturating_sub(second_keyframe);
        Some(ram)
    }

    /// RAM held by this ring's own payloads (save windows may hold more).
    fn own_ram(&self) -> usize {
        self.packets.iter().filter(|p| p.data.in_ram()).map(|p| p.data.len()).sum()
    }

    /// Push a packet and evict complete leading GOPs once elapsed time exceeds
    /// capacity. A save can consequently always begin with a decodable frame.
    pub fn push(&mut self, pkt: EncodedPacket) {
        if let Some(spill) = &self.spill {
            spill.track(&pkt.data);
        }
        self.bytes += pkt.data.len();
        self.packets.push_back(pkt);
        while self.span_us() > self.capacity_us {
            if self.drop_front_gop().is_none() {
                break;
            }
        }
        self.fit_ram();
    }

    /// Keep RAM within the spill budget: hand the oldest RAM payloads to the
    /// spill thread, and drop old GOPs when that cannot keep up or has failed,
    /// or when the spill file has wrapped over them.
    fn fit_ram(&mut self) {
        let Some(spill) = self.spill.clone() else {
            return;
        };
        // The spill file wrapped onto the oldest packets: they can no longer be saved.
        while self.packets.front().is_some_and(|p| p.data.lost()) {
            if self.drop_front_gop().is_none() {
                break;
            }
        }
        let budget = spill.budget();
        if spill.alive() {
            while spill.ram().saturating_sub(spill.pending()) > budget && self.spilled_to < self.packets.len() {
                let payload = self.packets[self.spilled_to].data.clone();
                self.spilled_to += 1;
                if payload.in_ram() && !spill.send(payload) {
                    break;
                }
            }
        }
        // Hard limit: the spill thread is behind or spilling is impossible. Only the
        // ring's own RAM counts here; a save in flight may briefly hold more.
        let hard = budget as usize + (budget as usize / 2).max(64 * 1024 * 1024);
        if spill.ram() as usize > hard {
            let mut own = self.own_ram();
            while own > hard {
                let Some(freed) = self.drop_front_gop() else { break };
                own = own.saturating_sub(freed);
                self.dropped_for_ram += 1;
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
            .filter(|(_, p)| p.keyframe && p.frame_idx <= target_start && !p.data.lost())
            .map(|(i, _)| i)
            .next_back()
            .or_else(|| {
                self.packets
                    .iter()
                    .enumerate()
                    .find(|(_, p)| p.keyframe && !p.data.lost())
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
            data: Payload::from(vec![0; 4]),
        }
    }

    /// A packet whose payload is unique to its PTS, so byte checks are meaningful.
    fn tagged(pts: u64, duration_us: u64, keyframe: bool) -> EncodedPacket {
        let tag = (pts / 1_000) as u8;
        EncodedPacket {
            frame_idx: pts,
            duration_us,
            keyframe,
            data: Payload::from(vec![tag, tag ^ 0x5a, tag.wrapping_add(1)]),
        }
    }

    #[test]
    fn window_shares_payloads_and_survives_eviction_and_tail_fixups() {
        let mut ring = PacketRing::new(200_000);
        for i in 0..6u64 {
            ring.push(tagged(i * 40_000, 40_000, i % 2 == 0));
        }
        let window = ring.save_window_at(120_000, 240_000);
        assert!(!window.is_empty());
        // Every window payload is the ring's own allocation, not a copy.
        for w in &window {
            assert!(
                ring.packets.iter().any(|r| r.data.same(&w.data)),
                "window payload at {} was deep-copied",
                w.frame_idx
            );
        }
        let frozen: Vec<(u64, u64, bool, Vec<u8>)> = window
            .iter()
            .map(|p| (p.frame_idx, p.duration_us, p.keyframe, p.data.to_vec()))
            .collect();
        let first_payload = window[0].data.clone();

        // The next packet refines the ring tail's duration (the window's last
        // packet), then later pushes evict every GOP the window came from.
        ring.set_last_duration_us(7);
        for i in 6..20u64 {
            if let Some(prev) = ring.last_pts_us() {
                ring.set_last_duration_us(i * 40_000 - prev);
            }
            ring.push(tagged(i * 40_000, 40_000, i % 2 == 0));
        }
        assert!(!ring
            .packets
            .iter()
            .any(|r| r.data.same(&first_payload)));
        let after: Vec<(u64, u64, bool, Vec<u8>)> = window
            .iter()
            .map(|p| (p.frame_idx, p.duration_us, p.keyframe, p.data.to_vec()))
            .collect();
        assert_eq!(after, frozen);
        // Only the window (and this test's extra handle) keep the evicted payload.
        assert_eq!(first_payload.holders(), 2);
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
