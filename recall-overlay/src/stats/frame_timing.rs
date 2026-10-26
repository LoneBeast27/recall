//! Frame timing for the stats island: owns the ETW session (`etw_session`)
//! while the stats are on, follows the target process, and turns its presents
//! into FPS + 1% low (`frame_math`).
//!
//! Lifecycle ([`decide`]): a session starts for the first target pid, is
//! re-aimed when the pid changes, stops when there is no target (and when the
//! poller is dropped: stats off, overlay exit). A refused start (the user is
//! neither an elevated administrator nor in Performance Log Users) is not
//! retried by this poller: membership only changes at the next sign-in, and
//! the overlay never prompts. Any other failure is retried after
//! [`RETRY_AFTER`].

use std::time::{Duration, Instant};

use serde::Serialize;

use super::etw_session::{EtwSession, StartError};
use super::frame_math::FrameStats;

pub const RETRY_AFTER: Duration = Duration::from_secs(5);

/// The overlay's own frame-timing state (diagnostics; the island's note).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FpsSource {
    /// Stats off (no session).
    #[default]
    Off,
    /// The ETW session runs for the target.
    Etw,
    /// `StartTrace` was refused: FPS shows "—" and the island says why.
    NoPermission,
    /// No game and no foreground window to measure.
    NoTarget,
    /// The session failed for another reason; retried shortly.
    Error,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phase {
    Idle,
    Running(u32),
    Denied,
    Failed(Instant),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Step {
    Nothing,
    Start(u32),
    Retarget(u32),
    Stop,
}

/// The session's next step for the wanted pid.
pub fn decide(phase: Phase, wanted: Option<u32>, now: Instant) -> Step {
    match (phase, wanted) {
        (Phase::Denied, _) => Step::Nothing,
        (Phase::Running(_), None) => Step::Stop,
        (Phase::Running(pid), Some(want)) if pid != want => Step::Retarget(want),
        (Phase::Running(_), Some(_)) => Step::Nothing,
        (Phase::Idle, Some(want)) => Step::Start(want),
        (Phase::Failed(at), Some(want)) if now.saturating_duration_since(at) >= RETRY_AFTER => Step::Start(want),
        (Phase::Idle | Phase::Failed(_), _) => Step::Nothing,
    }
}

/// What the island reports for a phase.
pub fn source_for(phase: Phase) -> FpsSource {
    match phase {
        Phase::Idle => FpsSource::NoTarget,
        Phase::Running(_) => FpsSource::Etw,
        Phase::Denied => FpsSource::NoPermission,
        Phase::Failed(_) => FpsSource::Error,
    }
}

/// Lives inside the stats poller; dropping it stops the session.
pub struct FrameTiming {
    phase: Phase,
    session: Option<EtwSession>,
}

impl FrameTiming {
    pub fn new() -> Self {
        Self { phase: Phase::Idle, session: None }
    }

    /// One 4 Hz poll for `wanted` (the target pid).
    pub fn poll(&mut self, wanted: Option<u32>) -> (FpsSource, FrameStats) {
        let now = Instant::now();
        if matches!(self.phase, Phase::Running(_)) && !self.session.as_ref().is_some_and(EtwSession::alive) {
            eprintln!("recall-overlay: frame timing session ended; restarting");
            self.session = None;
            self.phase = Phase::Failed(now);
        }
        match decide(self.phase, wanted, now) {
            Step::Nothing => {}
            Step::Start(pid) => self.start(pid, now),
            Step::Retarget(pid) => {
                if let Some(session) = self.session.as_mut() {
                    session.retarget(pid);
                }
                self.phase = Phase::Running(pid);
            }
            Step::Stop => {
                self.session = None;
                self.phase = Phase::Idle;
            }
        }
        let stats = self.session.as_ref().map(EtwSession::stats).unwrap_or_default();
        (source_for(self.phase), stats)
    }

    fn start(&mut self, pid: u32, now: Instant) {
        self.phase = match EtwSession::start(pid) {
            Ok(session) => {
                self.session = Some(session);
                Phase::Running(pid)
            }
            Err(StartError::Denied) => {
                eprintln!("recall-overlay: frame timing refused (not in Performance Log Users); FPS shows —");
                Phase::Denied
            }
            Err(StartError::Failed(why)) => {
                eprintln!("recall-overlay: frame timing failed: {why}");
                Phase::Failed(now)
            }
        };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_session_starts_for_a_target_and_follows_it() {
        let now = Instant::now();
        assert_eq!(decide(Phase::Idle, Some(42), now), Step::Start(42));
        assert_eq!(decide(Phase::Running(42), Some(42), now), Step::Nothing);
        assert_eq!(decide(Phase::Running(42), Some(7), now), Step::Retarget(7));
        assert_eq!(decide(Phase::Running(42), None, now), Step::Stop, "no target: no session");
        assert_eq!(decide(Phase::Idle, None, now), Step::Nothing);
    }

    #[test]
    fn a_refused_start_is_never_retried_and_never_prompts() {
        let now = Instant::now();
        for wanted in [None, Some(42), Some(7)] {
            assert_eq!(decide(Phase::Denied, wanted, now + Duration::from_secs(600)), Step::Nothing);
        }
        assert_eq!(source_for(Phase::Denied), FpsSource::NoPermission);
    }

    #[test]
    fn other_failures_retry_after_a_pause() {
        let failed = Instant::now();
        let phase = Phase::Failed(failed);
        assert_eq!(decide(phase, Some(42), failed + RETRY_AFTER - Duration::from_millis(1)), Step::Nothing);
        assert_eq!(decide(phase, Some(42), failed + RETRY_AFTER), Step::Start(42));
        assert_eq!(decide(phase, None, failed + RETRY_AFTER), Step::Nothing);
        assert_eq!(source_for(phase), FpsSource::Error);
    }

    #[test]
    fn sources_name_every_phase_for_the_island_and_diagnostics() {
        assert_eq!(source_for(Phase::Running(1)), FpsSource::Etw);
        assert_eq!(source_for(Phase::Idle), FpsSource::NoTarget);
        assert_eq!(FpsSource::default(), FpsSource::Off);
        let names: Vec<String> = [FpsSource::Off, FpsSource::Etw, FpsSource::NoPermission, FpsSource::NoTarget, FpsSource::Error]
            .iter()
            .map(|s| serde_json::to_value(s).unwrap().as_str().unwrap().to_owned())
            .collect();
        assert_eq!(names, ["off", "etw", "no_permission", "no_target", "error"]);
    }
}
