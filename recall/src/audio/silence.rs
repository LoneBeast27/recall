//! Silence on a quiet track, and the clip cut that never depends on when it was written.
//!
//! A quiet track's owner thread writes the silence it owes the clock (`Leveller::idle`) at least
//! once a second ([`QUIET_FILL`]), in one block (`VecDeque::resize`, one long-recording item),
//! and notes up to where it wrote ([`QuietMark`], under the ring lock). A save's snapshot pads
//! the rest up to "now" itself from that mark, so a save never waits for the owner and a late
//! owner (busy, or stopped by the privacy stop) never moves the clip's audio.

use std::time::{Duration, Instant};

use super::TrackRing;

/// A quiet track writes its silence at least this often (the snapshot pads the rest).
/// timer-ok: the ring has to stay near the clock for the long recording; once a second is
/// the bound the save no longer depends on.
pub(crate) const QUIET_FILL: Duration = Duration::from_secs(1);

/// The ring holds silence up to `written` frames of the owner's clock (`origin` = its time 0):
/// every sample from there to now is silence too.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct QuietMark {
    pub origin: Instant,
    pub written: u64,
}

impl QuietMark {
    /// Frames owed at `now` (`Leveller::due` at the ring's rate, minus what was written).
    pub(super) fn owed(&self, rate: u32, now: Instant) -> u64 {
        let due = (now.saturating_duration_since(self.origin).as_secs_f64() * rate as f64) as u64;
        due.saturating_sub(self.written)
    }
}

impl TrackRing {
    /// `frames` of silence in one block, then `mark` (None: leave it). The privacy stop stores
    /// nothing and keeps the old mark, so a snapshot still pads up to now from it.
    pub(super) fn push_silence_frames(&self, frames: u64, mark: Option<QuietMark>) {
        if crate::stop_capture::stopped() {
            return;
        }
        let ch = self.channels.max(1) as usize;
        let n = (frames.min(self.capacity_frames()) as usize) * ch;
        if n > 0 {
            crate::longrec::tap::tap_silence(self.name, n); // long-recording hook: one item
        }
        let mut buf = self.samples.lock().expect("ring lock");
        let over = (buf.len() + n).saturating_sub(self.capacity);
        buf.drain(..over);
        let len = buf.len();
        buf.resize(len + n, 0.0);
        if mark.is_some() {
            *self.live.quiet.lock().unwrap_or_else(|p| p.into_inner()) = mark;
        }
    }

    /// Live sound follows: the ring is level with the clock again as packets come.
    pub(super) fn clear_quiet(&self) {
        if crate::stop_capture::stopped() {
            return; // nothing enters after the stop: the mark still says where silence began
        }
        let _buf = self.samples.lock().expect("ring lock");
        *self.live.quiet.lock().unwrap_or_else(|p| p.into_inner()) = None;
    }

    /// Snapshot the newest `secs` of audio that end `tail_secs` before now, front-padded with
    /// silence to exactly `secs` so all tracks align at the clip start. A quiet track's ring
    /// may stop short of now (its owner writes once a second): the silence owed since its mark
    /// is appended here, so the window always ends at now.
    ///
    /// The capture thread shares the ring lock, so the critical section is only the ring's two
    /// contiguous halves `memcpy`d into pages faulted in before the lock was taken.
    pub fn snapshot_ending(&self, secs: f64, tail_secs: f64) -> Vec<f32> {
        self.snapshot_ending_at(secs, tail_secs, Instant::now())
    }

    /// [`Self::snapshot_ending`] with "now" given (tests).
    pub(super) fn snapshot_ending_at(&self, secs: f64, tail_secs: f64, now: Instant) -> Vec<f32> {
        let ch = self.channels as usize;
        let want = (secs * self.rate as f64) as usize * ch;
        let tail = (tail_secs.max(0.0) * self.rate as f64).round() as usize * ch;
        // Zeroed allocation doubles as the silence padding. Touch one sample per page so the
        // copy below never takes demand-zero page faults under the lock (black_box keeps the
        // store from being elided as redundant).
        let mut out = vec![0.0f32; want];
        const PAGE_SAMPLES: usize = 4096 / std::mem::size_of::<f32>();
        for i in (0..want).step_by(PAGE_SAMPLES) {
            out[i] = std::hint::black_box(0.0);
        }
        let buf = self.samples.lock().expect("ring lock");
        let mark = *self.live.quiet.lock().unwrap_or_else(|p| p.into_inner());
        let owed = mark.map_or(0, |m| m.owed(self.rate, now) as usize * ch);
        // The ring then `owed` zeros: positions [start, end) of that, end = its length minus the tail.
        let end = (buf.len() + owed).saturating_sub(tail);
        let start = end.saturating_sub(want);
        let ring_end = end.min(buf.len());
        if ring_end > start {
            let dst = &mut out[want - (end - start)..][..ring_end - start];
            let (head, back) = buf.as_slices();
            let h0 = start.min(head.len());
            let h1 = ring_end.min(head.len());
            let from_head = &head[h0..h1];
            dst[..from_head.len()].copy_from_slice(from_head);
            let b0 = start.saturating_sub(head.len());
            let b1 = ring_end.saturating_sub(head.len());
            dst[from_head.len()..].copy_from_slice(&back[b0..b1]);
        }
        drop(buf);
        out
    }
}

#[cfg(test)]
#[path = "silence_tests.rs"]
mod tests;
