//! Per-packet work on a capture thread with no heap allocation once it runs (gate review P2-c):
//! a packet's bytes decode into a buffer the stream keeps, and silence is one ring resize
//! (`silence.rs`). `TrackRing::push_samples` keeps its gain buffer per thread, and the long
//! recording's tap recycles its buffers (`longrec::tap`).

use std::collections::VecDeque;


/// The whole `block`-byte frames of `raw` (little-endian f32 samples) into `out`, which is
/// cleared first and grows only until it fits the largest packet.
pub(super) fn decode(raw: &mut VecDeque<u8>, block: usize, out: &mut Vec<f32>) {
    let full = raw.len() - raw.len() % block.max(4);
    out.clear();
    let mut bytes = raw.drain(..full);
    while let (Some(a), Some(b), Some(c), Some(d)) = (bytes.next(), bytes.next(), bytes.next(), bytes.next()) {
        out.push(f32::from_le_bytes([a, b, c, d]));
    }
}

#[cfg(test)]
#[path = "packet_tests.rs"]
mod tests;
