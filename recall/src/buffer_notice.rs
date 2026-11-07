//! When the controller hears `buffer_restarted` (its "HDR turned on" / "The replay buffer
//! restarted" notice). A capture restart for HDR (a Bgra8 capture on a <= 4 GiB card,
//! `capture::format`) does not by itself reset the replay buffer: the encoder reopens on the
//! new capture's frames and keeps the ring when the size holds (`encoder/live.rs`, "the
//! replay buffer is kept"). So the capture side only notes the HDR restart, and the
//! encoder's own decision on the new capture's first frame says whether the ring was reset.
//!
//! Only a reset that follows an HDR restart is announced (within [`WINDOW`]); other size
//! resets (a game changing resolution) stay log-only, as before.

use std::sync::Mutex;
use std::time::{Duration, Instant};

use crate::capture::format::HDR_TURNED_ON;

/// The `buffer_restarted` reason for a ring reset after HDR turned on.
pub(crate) const HDR_ON: &str = "hdr_on";
/// How long an HDR capture restart waits for the encoder's decision.
pub(crate) const WINDOW: Duration = Duration::from_secs(10);

/// An HDR capture restart waiting for the encoder's decision.
#[derive(Debug, Default)]
pub(crate) struct BufferNotice {
    hdr_restart: Option<Instant>,
}

impl BufferNotice {
    /// The capture is replaced for `why`. Returns a `buffer_restarted` reason to send now.
    pub(crate) fn capture_replaced(&mut self, why: &str, now: Instant) -> Option<&'static str> {
        if why == HDR_TURNED_ON {
            self.hdr_restart = Some(now);
        }
        None // the ring is not reset yet: the encoder decides on the new capture's frames
    }

    /// The encoder met a new capture's input: `ring_reset` = it reset the replay buffer
    /// (size change), false = it kept it. Returns a `buffer_restarted` reason to send.
    pub(crate) fn encoder_input(&mut self, ring_reset: bool, now: Instant) -> Option<&'static str> {
        let hdr = self.hdr_restart.take().is_some_and(|t| now.saturating_duration_since(t) <= WINDOW);
        (hdr && ring_reset).then_some(HDR_ON)
    }
}

/// The engine's one notice state (capture thread side and encoder thread side).
static NOTICE: Mutex<BufferNotice> = Mutex::new(BufferNotice { hdr_restart: None });

/// Sends `buffer_restarted` for `reason`, if any (JSON mode: the controller is listening).
pub(crate) fn send(json: bool, reason: Option<&'static str>) {
    if let Some(reason) = reason.filter(|_| json) {
        crate::protocol::emit(&crate::protocol::Outbound::BufferRestarted { reason });
    }
}

/// [`BufferNotice::capture_replaced`] on the engine's state, at `Instant::now()`.
pub(crate) fn capture_replaced(why: &str) -> Option<&'static str> {
    NOTICE.lock().unwrap_or_else(|p| p.into_inner()).capture_replaced(why, Instant::now())
}

/// [`BufferNotice::encoder_input`] on the engine's state, at `Instant::now()`.
#[cfg_attr(not(feature = "encode"), allow(dead_code))]
pub(crate) fn encoder_input(ring_reset: bool) -> Option<&'static str> {
    NOTICE.lock().unwrap_or_else(|p| p.into_inner()).encoder_input(ring_reset, Instant::now())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Everything one HDR flip sends: the capture restart, then the encoder's decision.
    fn flip(n: &mut BufferNotice, ring_reset: bool, t0: Instant, decided_after: Duration) -> Vec<&'static str> {
        let mut sent: Vec<_> = n.capture_replaced(HDR_TURNED_ON, t0).into_iter().collect();
        sent.extend(n.encoder_input(ring_reset, t0 + decided_after));
        sent
    }

    #[test]
    fn an_hdr_flip_on_bgra8_at_the_same_size_sends_no_buffer_restarted() {
        let mut n = BufferNotice::default();
        assert!(flip(&mut n, false, Instant::now(), Duration::from_millis(200)).is_empty(), "the ring was kept");
    }

    #[test]
    fn an_hdr_flip_that_resets_the_ring_announces_once() {
        let mut n = BufferNotice::default();
        let t0 = Instant::now();
        assert_eq!(flip(&mut n, true, t0, Duration::from_millis(200)), vec![HDR_ON]);
        assert_eq!(n.encoder_input(true, t0 + Duration::from_secs(1)), None, "a later reset is not HDR's");
    }

    #[test]
    fn a_reset_without_an_hdr_restart_or_long_after_it_stays_quiet() {
        let mut n = BufferNotice::default();
        let t0 = Instant::now();
        assert_eq!(n.encoder_input(true, t0), None, "a game's own resolution change");
        assert_eq!(n.capture_replaced("(capture source changed)", t0), None);
        assert_eq!(n.encoder_input(true, t0), None, "not an HDR restart");
        assert_eq!(flip(&mut n, true, t0, WINDOW + Duration::from_secs(1)), Vec::<&str>::new(), "too late to be HDR's");
    }
}
