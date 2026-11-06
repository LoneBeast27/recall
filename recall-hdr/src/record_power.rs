//! Sleep and resume (audit T1). Windows' clock runs on through sleep, so an engine that
//! sleeps with the PC wakes up owing the whole sleep: every audio track, and a running long
//! recording, would get hours of silence and a frozen frame. The controller answers the
//! power notices instead:
//! - Suspend: a running long recording is stopped first, so its file ends cleanly before
//!   the sleep (a fragmented MP4 also plays up to its last fragment if the stop is cut short).
//! - Resume (`PBT_APMRESUMEAUTOMATIC`, sent once per wake): the engine restarts as an
//!   intended restart (a fresh replay buffer, not counted against the supervisor's give-up),
//!   and the foreground is read again as if it were new.
//!
//! This module only decides; `record_power_win` registers for the notices and applies the
//! actions on the pump thread.

/// `PBT_APMSUSPEND`.
pub const PBT_SUSPEND: u32 = 4;
/// `PBT_APMRESUMEAUTOMATIC`: every wake, with or without the user.
pub const PBT_RESUME_AUTOMATIC: u32 = 18;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PowerEvent {
    Suspend,
    Resume,
}

impl PowerEvent {
    /// The notices that matter; the rest (`PBT_APMRESUMESUSPEND`, power-source changes,
    /// setting changes) are ignored.
    pub fn from_pbt(code: u32) -> Option<Self> {
        match code {
            PBT_SUSPEND => Some(Self::Suspend),
            PBT_RESUME_AUTOMATIC => Some(Self::Resume),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PowerAction {
    /// Send the engine `record_stop`.
    StopRecording,
    /// Restart the engine as an intended restart.
    RestartEngine,
    /// Forget the last foreground fed to the tracker and probe it again.
    Reprobe,
}

/// What each power notice asks of the controller. One restart per wake: a second resume
/// with no suspend between (a repeated notice) does nothing.
#[derive(Debug, Default)]
pub struct PowerPlanner {
    restarted_since_suspend: bool,
}

impl PowerPlanner {
    pub fn on_event(&mut self, event: PowerEvent, recording: bool) -> Vec<PowerAction> {
        match event {
            PowerEvent::Suspend => {
                self.restarted_since_suspend = false;
                if recording { vec![PowerAction::StopRecording] } else { Vec::new() }
            }
            PowerEvent::Resume if self.restarted_since_suspend => Vec::new(),
            PowerEvent::Resume => {
                self.restarted_since_suspend = true;
                vec![PowerAction::RestartEngine, PowerAction::Reprobe]
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{PowerAction, PowerEvent, PowerPlanner, PBT_RESUME_AUTOMATIC, PBT_SUSPEND};

    #[test]
    fn suspend_during_a_long_recording_stops_it() {
        let mut planner = PowerPlanner::default();
        assert_eq!(planner.on_event(PowerEvent::Suspend, true), vec![PowerAction::StopRecording]);
        assert_eq!(planner.on_event(PowerEvent::Suspend, false), vec![], "nothing to stop");
    }

    #[test]
    fn resume_restarts_the_engine_once_and_re_probes() {
        let mut planner = PowerPlanner::default();
        planner.on_event(PowerEvent::Suspend, false);
        assert_eq!(planner.on_event(PowerEvent::Resume, false), vec![PowerAction::RestartEngine, PowerAction::Reprobe]);
        assert_eq!(planner.on_event(PowerEvent::Resume, false), vec![], "a repeated resume notice");
        planner.on_event(PowerEvent::Suspend, false);
        assert_eq!(planner.on_event(PowerEvent::Resume, true).len(), 2, "the next wake restarts again");
    }

    #[test]
    fn a_resume_whose_suspend_was_missed_still_restarts() {
        let mut planner = PowerPlanner::default();
        assert_eq!(planner.on_event(PowerEvent::Resume, false), vec![PowerAction::RestartEngine, PowerAction::Reprobe]);
    }

    #[test]
    fn only_suspend_and_automatic_resume_are_acted_on() {
        assert_eq!(PowerEvent::from_pbt(PBT_SUSPEND), Some(PowerEvent::Suspend));
        assert_eq!(PowerEvent::from_pbt(PBT_RESUME_AUTOMATIC), Some(PowerEvent::Resume));
        assert_eq!(PowerEvent::from_pbt(7), None, "PBT_APMRESUMESUSPEND follows the automatic one");
        assert_eq!(PowerEvent::from_pbt(10), None, "power source changes");
    }
}
