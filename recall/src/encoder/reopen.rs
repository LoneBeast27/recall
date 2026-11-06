//! When a failed encoder may open again (pure: the encoder thread passes the clock in).
//!
//! A fatal encode error or a failed open marks the input (frame size + frames pool)
//! failed. The same input is tried again after 2 s, 5 s, 10 s, then every 30 s; a frame of
//! another size or from another pool (a capture restart brings a new D3D11 device) gets an
//! attempt at once. Every [`RESTART_EVERY`] (3) failed reopens the encoder asks for a
//! capture restart: after DXGI device-removed only a new capture device lets NVENC open
//! again. The first ask comes about 17 s after the error (2 + 5 + 10 s). An encoder that
//! opens and fails again at once stays in the same episode (no 2 s loop); one minute of
//! clean encoding ends it.

use std::time::{Duration, Instant};

/// Wait before each retry of the same input: 2 s, 5 s, 10 s, then every 30 s.
pub(crate) const RETRY_DELAYS: [Duration; 4] =
    [Duration::from_secs(2), Duration::from_secs(5), Duration::from_secs(10), Duration::from_secs(30)];
/// Every this many failed reopens the encoder asks for a capture restart.
pub(crate) const RESTART_EVERY: u32 = 3;
/// GPU frames that still fail after this many reopens (a capture restart came in between)
/// fall back to CPU copies, as a first open refused by NVENC does.
pub(crate) const CPU_FALLBACK_AFTER: u32 = 6;
/// An encoder that has run this long without an error ends the failure episode.
pub(crate) const STABLE_AFTER: Duration = Duration::from_secs(60);

/// What the encoder opens on: the frame size and the D3D11 frames pool (`None` = RGBA).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Input {
    pub width: u32,
    pub height: u32,
    pub pool: Option<usize>,
}

/// What follows a failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AfterFail {
    /// Retry the same input after this long.
    Wait(Duration),
    /// Retry after this long, and ask for a capture restart now.
    RestartCapture(Duration),
}

impl AfterFail {
    pub fn delay(self) -> Duration {
        match self {
            AfterFail::Wait(d) | AfterFail::RestartCapture(d) => d,
        }
    }
}

#[derive(Debug, Default)]
pub(crate) struct Reopen {
    /// The input that failed and when it may be tried again (`None` = not waiting).
    retry: Option<(Input, Instant)>,
    /// A failure episode is open: later failures count as failed reopens.
    episode: bool,
    /// Failed reopens in this episode (the error that opened it is not one).
    reopen_failures: u32,
    /// When the encoder last opened inside the episode.
    opened_at: Option<Instant>,
}

impl Reopen {
    /// Whether the encoder may try to open on `input` now.
    pub fn may_open(&self, input: Input, now: Instant) -> bool {
        self.retry.is_none_or(|(failed, at)| failed != input || now >= at)
    }

    /// Inside a failure episode (a GPU open failure then counts, it does not refuse GPU frames).
    pub fn recovering(&self) -> bool {
        self.episode
    }

    pub fn reopen_failures(&self) -> u32 {
        self.reopen_failures
    }

    /// The encoder failed on `input` (an encode error, or an open that did not work).
    pub fn failed(&mut self, input: Input, now: Instant) -> AfterFail {
        if self.episode {
            self.reopen_failures = self.reopen_failures.saturating_add(1);
        } else {
            self.episode = true;
            self.reopen_failures = 0;
        }
        let n = self.reopen_failures as usize;
        let delay = RETRY_DELAYS[n.min(RETRY_DELAYS.len() - 1)];
        self.retry = Some((input, now + delay));
        self.opened_at = None;
        if n > 0 && self.reopen_failures.is_multiple_of(RESTART_EVERY) {
            AfterFail::RestartCapture(delay)
        } else {
            AfterFail::Wait(delay)
        }
    }

    /// The encoder opened: stop waiting (the episode lasts until it has run cleanly).
    pub fn opened(&mut self, now: Instant) {
        self.retry = None;
        self.opened_at = self.episode.then_some(now);
    }

    /// A frame encoded without an error.
    pub fn encoded(&mut self, now: Instant) {
        if self.opened_at.is_some_and(|at| now.saturating_duration_since(at) >= STABLE_AFTER) {
            *self = Reopen::default();
        }
    }
}

/// A fatal encode error on `input`: retry later (the encoder thread's live path).
pub(super) fn encode_failed(reopen: &mut Reopen, input: Input, stats: &super::EncStats, json: bool) {
    let after = reopen.failed(input, Instant::now());
    after_failure(after, reopen.reopen_failures(), stats, json);
}

/// An open failed. A first open refused on GPU frames lets the CPU frames that follow open
/// the encoder instead (capture switches for good); during a recovery a GPU failure is a
/// failed reopen, and only [`CPU_FALLBACK_AFTER`] of them switch to CPU copies.
pub(super) fn open_failed(reopen: &mut Reopen, input: Input, gpu: bool, stats: &super::EncStats, json: bool) {
    if !gpu || reopen.recovering() {
        encode_failed(reopen, input, stats, json);
    }
    if gpu && (!reopen.recovering() || reopen.reopen_failures() >= CPU_FALLBACK_AFTER) {
        stats.gpu_refused.store(true, std::sync::atomic::Ordering::Relaxed);
        super::report(json, "NVENC refused GPU frames; capture switches to CPU copies");
    }
}

/// Log a failure's retry and ask for a capture restart when it says so.
fn after_failure(after: AfterFail, failures: u32, stats: &super::EncStats, json: bool) {
    super::report(json, &format!("encoder retries in {} s", after.delay().as_secs()));
    if let AfterFail::RestartCapture(_) = after {
        super::report(json, &format!("encoder failed to reopen {failures} times; asking for a capture restart"));
        stats.request_capture_restart();
    }
}

#[cfg(test)]
#[path = "reopen_tests.rs"]
mod tests;
