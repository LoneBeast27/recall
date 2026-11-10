//! Gate review P2-c: the capture thread's steady per-packet work allocates nothing, for a
//! gained packet and for silence up to the clock (a quiet or muted source). Counted with the
//! test allocator (`test_alloc`) on this thread only; the ring is warm (full) first.

use std::cell::Cell;
use std::rc::Rc;
use std::time::Duration;

use super::tests::{ring, FakeClock};
use super::*;
use crate::audio::MuteNow;
use crate::test_alloc::bytes_in;

#[test]
fn steady_gained_packets_and_silence_allocate_nothing() {
    let ring = ring();
    ring.set_gain_db(6.0);
    let (packet, now) = ([0.25f32; 2], MuteNow { muted: false, presses: 0 });
    for _ in 0..ring.capacity {
        ring.push_live(&packet, now);
    }
    let ((), bytes) = bytes_in(|| (0..100).for_each(|_| ring.push_live(&packet, now)));
    assert_eq!(bytes, 0, "a gained packet allocates nothing");

    let at = Rc::new(Cell::new(Duration::from_secs(1)));
    let clock = FakeClock { now: at.clone(), stop_at: Duration::MAX };
    let mut level = Leveller::new(ring.rate).with_cap(ring.capacity_frames());
    silence_to_clock(&ring, &mut level, &clock);
    let ((), bytes) = bytes_in(|| {
        for secs in 2..50 {
            at.set(Duration::from_secs(secs));
            silence_to_clock(&ring, &mut level, &clock);
        }
    });
    assert_eq!(bytes, 0, "silence up to the clock allocates nothing");
}
