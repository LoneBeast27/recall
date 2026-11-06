//! When a failed encoder may open again (pure: the encoder thread passes the clock in).
//!
//! A fatal encode error or a failed open marks the input (frame size + frames pool)
//! failed; the replay buffer is kept (`replay.rs`). The same input is tried again after
//! 2 s, 5 s, 10 s, then every 30 s; a frame of another size or from another pool (a
//! capture restart brings a new D3D11 device) gets an attempt at once. When no frame comes
//! to try (a dead capture), a retry that is a further 2 s overdue counts as failed, so the
//! plan moves on without frames.
//!
//! The engine's own plan, in order: retry in place; every [`RESTART_EVERY`] (3) failed
//! reopens ask for a capture restart (after DXGI device-removed only a new device lets
//! NVENC open), the first about 17 s after the error; a GPU open that still fails after
//! that ([`CPU_FALLBACK_AFTER`]) falls back to CPU copies. Only after
//! [`HAND_OFF_AFTER`] failed reopens, or [`HAND_OFF_AGE`] without an encoder, does status
//! say `failed`, which is what the controller's engine restart waits for; until then it
//! says `recovering`. An encoder that opens and fails again at once stays in the same
//! episode (no 2 s loop); one minute of clean encoding ends it.
//!
//! [`GpuProbe`]: CPU copies are not for good. The GPU path is tried again after 1 min,
//! 5 min, 15 min, then every 30 min.

use std::sync::atomic::Ordering::Relaxed;
use std::time::{Duration, Instant};

/// Wait before each retry of the same input: 2 s, 5 s, 10 s, then every 30 s.
pub(crate) const RETRY_DELAYS: [Duration; 4] =
    [Duration::from_secs(2), Duration::from_secs(5), Duration::from_secs(10), Duration::from_secs(30)];
/// Every this many failed reopens the encoder asks for a capture restart.
pub(crate) const RESTART_EVERY: u32 = 3;
/// A GPU open that fails this many reopens in (the one after the capture restart) falls
/// back to CPU copies, as a first open refused by NVENC does.
pub(crate) const CPU_FALLBACK_AFTER: u32 = 4;
/// The engine's plan is spent: status says `failed` (the controller may restart the engine).
pub(crate) const HAND_OFF_AFTER: u32 = 5;
/// ...or once an episode has gone this long without an encoder.
pub(crate) const HAND_OFF_AGE: Duration = Duration::from_secs(60);
/// An encoder that has run this long without an error ends the failure episode.
pub(crate) const STABLE_AFTER: Duration = Duration::from_secs(60);
/// After a GPU refusal, try the GPU path again after these: 1 min, 5 min, 15 min, then 30 min.
pub(crate) const PROBE_DELAYS: [Duration; 4] =
    [Duration::from_secs(60), Duration::from_secs(300), Duration::from_secs(900), Duration::from_secs(1800)];

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

#[derive(Debug)]
pub(crate) struct Reopen {
    /// `RETRY_DELAYS` (tests shorten them).
    delays: [Duration; 4],
    /// The input that failed and when it may be tried again (`None` = not waiting).
    retry: Option<(Input, Instant)>,
    /// When the open failure episode began (`None` = none open).
    since: Option<Instant>,
    /// Failed reopens in this episode (the error that opened it is not one).
    reopen_failures: u32,
    /// When the encoder last opened inside the episode.
    opened_at: Option<Instant>,
}

impl Default for Reopen {
    fn default() -> Self {
        Self::with_delays(RETRY_DELAYS)
    }
}

impl Reopen {
    pub fn with_delays(delays: [Duration; 4]) -> Self {
        Self { delays, retry: None, since: None, reopen_failures: 0, opened_at: None }
    }

    /// Whether the encoder may try to open on `input` now.
    pub fn may_open(&self, input: Input, now: Instant) -> bool {
        self.retry.is_none_or(|(failed, at)| failed != input || now >= at)
    }

    /// Inside a failure episode (a GPU open failure then counts, it does not refuse GPU frames).
    pub fn recovering(&self) -> bool {
        self.since.is_some()
    }

    pub fn reopen_failures(&self) -> u32 {
        self.reopen_failures
    }

    /// The engine's own plan is spent: status `failed` instead of `recovering`.
    pub fn handed_off(&self, now: Instant) -> bool {
        self.since.is_some_and(|since| {
            self.reopen_failures >= HAND_OFF_AFTER || (self.retry.is_some() && now.saturating_duration_since(since) >= HAND_OFF_AGE)
        })
    }

    /// The encoder failed on `input` (an encode error, or an open that did not work).
    pub fn failed(&mut self, input: Input, now: Instant) -> AfterFail {
        if self.since.is_some() {
            self.reopen_failures = self.reopen_failures.saturating_add(1);
        } else {
            self.since = Some(now);
            self.reopen_failures = 0;
        }
        let n = self.reopen_failures as usize;
        let delay = self.delays[n.min(self.delays.len() - 1)];
        self.retry = Some((input, now + delay));
        self.opened_at = None;
        if n > 0 && self.reopen_failures.is_multiple_of(RESTART_EVERY) {
            AfterFail::RestartCapture(delay)
        } else {
            AfterFail::Wait(delay)
        }
    }

    /// A due retry no frame came to try: it counts as failed.
    pub fn stalled(&mut self, now: Instant) -> Option<AfterFail> {
        let (input, at) = self.retry?;
        (now >= at + self.delays[0]).then(|| self.failed(input, now))
    }

    /// The encoder opened: stop waiting (the episode lasts until it has run cleanly).
    pub fn opened(&mut self, now: Instant) {
        self.retry = None;
        self.opened_at = self.since.is_some().then_some(now);
    }

    /// A frame encoded without an error.
    pub fn encoded(&mut self, now: Instant) {
        if self.opened_at.is_some_and(|at| now.saturating_duration_since(at) >= STABLE_AFTER) {
            *self = Reopen::with_delays(self.delays);
        }
    }
}

/// When to try the GPU path again after GPU frames were refused.
#[derive(Debug)]
pub(crate) struct GpuProbe {
    delays: [Duration; 4],
    next: Option<Instant>,
    tries: usize,
}

impl Default for GpuProbe {
    fn default() -> Self {
        Self::with_delays(PROBE_DELAYS)
    }
}

impl GpuProbe {
    pub fn with_delays(delays: [Duration; 4]) -> Self {
        Self { delays, next: None, tries: 0 }
    }

    /// GPU frames were refused (again): schedule the next try.
    pub fn refused(&mut self, now: Instant) {
        self.next = Some(now + self.delays[self.tries.min(self.delays.len() - 1)]);
        self.tries += 1;
    }

    /// Time to try the GPU path again (once per scheduled try).
    pub fn due(&mut self, now: Instant) -> bool {
        let due = self.next.is_some_and(|at| now >= at);
        if due {
            self.next = None;
        }
        due
    }

    /// The GPU path works again: a later refusal starts at 1 min.
    pub fn gpu_ok(&mut self) {
        *self = GpuProbe::with_delays(self.delays);
    }
}

/// A fatal encode error on `input`: retry later (the encoder thread's live path).
pub(super) fn encode_failed(reopen: &mut Reopen, input: Input, stats: &super::EncStats, json: bool) {
    let after = reopen.failed(input, Instant::now());
    after_failure(after, reopen.reopen_failures(), stats, json);
}

/// An open failed. A first open refused on GPU frames lets the CPU frames that follow open
/// the encoder instead (capture switches until the next [`GpuProbe`] try); during a
/// recovery a GPU failure is a failed reopen, and only [`CPU_FALLBACK_AFTER`] of them
/// switch to CPU copies. True when GPU frames were refused now.
pub(super) fn open_failed(reopen: &mut Reopen, input: Input, gpu: bool, stats: &super::EncStats, json: bool) -> bool {
    if !gpu || reopen.recovering() {
        encode_failed(reopen, input, stats, json);
    }
    let refuse = gpu && (!reopen.recovering() || reopen.reopen_failures() >= CPU_FALLBACK_AFTER);
    if refuse {
        stats.gpu_refused.store(true, Relaxed);
        super::report(json, "NVENC refused GPU frames; capture switches to CPU copies");
    }
    refuse
}

/// Log a failure's retry and ask for a capture restart when it says so.
pub(super) fn after_failure(after: AfterFail, failures: u32, stats: &super::EncStats, json: bool) {
    super::report(json, &format!("encoder retries in {:.1} s", after.delay().as_secs_f32()));
    if let AfterFail::RestartCapture(_) = after {
        super::report(json, &format!("encoder failed to reopen {failures} times; asking for a capture restart"));
        stats.request_capture_restart();
    }
}

#[cfg(test)]
#[path = "reopen_tests.rs"]
mod tests;
