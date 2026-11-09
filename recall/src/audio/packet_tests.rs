//! Gate review P2-c: a WASAPI packet's bytes decode into a kept buffer, levelled into another,
//! and the long recording's copy comes from a recycled buffer: once warm, a packet allocates
//! nothing on the capture thread (test allocator, this thread only).

use std::collections::VecDeque;
use std::time::Duration;

use super::*;
use crate::audio::apps::Leveller;
use crate::longrec::tap::{fill, recycle};
use crate::test_alloc::bytes_in;

fn bytes(samples: &[f32]) -> Vec<u8> {
    samples.iter().flat_map(|s| s.to_le_bytes()).collect()
}

#[test]
fn whole_frames_decode_and_a_partial_one_waits() {
    let mut raw: VecDeque<u8> = bytes(&[0.5, -0.25, 1.0]).into_iter().collect();
    raw.push_back(7); // a frame's first byte only
    let mut out = Vec::new();
    decode(&mut raw, 8, &mut out); // stereo f32: 8-byte frames
    assert_eq!(out, [0.5, -0.25], "one whole frame");
    assert_eq!(raw.len(), 5, "the partial frame stays for the next packet");
}

#[test]
fn a_steady_packet_allocates_nothing() {
    let packet = bytes(&[0.25; 960]);
    let (mut raw, mut samples, mut leveled) = (VecDeque::new(), Vec::new(), Vec::new());
    let mut level = Leveller::new(48_000).with_cap(48_000 * 60);
    let mut step = |n: u64, raw: &mut VecDeque<u8>| {
        raw.extend(packet.iter().copied());
        decode(raw, 8, &mut samples);
        level.live_into(Duration::from_millis(10 * n), &samples, 2, &mut leveled);
        recycle(fill(&leveled, false));
        recycle(fill(&leveled, true));
    };
    (1..50).for_each(|n| step(n, &mut raw));
    let ((), allocated) = bytes_in(|| (50..150).for_each(|n| step(n, &mut raw)));
    assert_eq!(allocated, 0, "decode, level and the tap copy reuse their buffers");
}
