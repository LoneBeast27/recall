//! Sleep and resume (audit T1). Windows' clock runs on through sleep, so an engine that
//! sleeps with the PC wakes up owing the whole sleep: every audio track, and a running long
//! recording, would get hours of silence and a frozen frame. The controller answers the
//! power notices instead:
//! - Suspend: a running long recording is stopped first, so its file ends cleanly before
//!   the sleep (a fragmented MP4 also plays up to its last fragment if the stop is cut short).
//! - Resume (`PBT_APMRESUMEAUTOMATIC`, sent once per wake): the engine restarts as an
//!   intended restart (a fresh replay buffer, not counted against the supervisor's give-up),
//!   and the foreground is read again as if it were new. While the engine is still writing a
//!   save (`EngineActivity::saves_writing`), that restart and re-probe wait for it
//!   ([`PowerPlanner::gate`]): the restart would abort the save. The suspend is never held.
//!
//! This module only decides; `record_power_win` registers for the notices and applies the
//! actions on the pump thread.
//!
//! The suspend stop is best-effort: the system's power callback only queues the notice and
//! wakes the pump, it never waits, so a PC that sleeps within a pump tick of the notice can
//! sleep before `record_stop` lands. The fragmented MP4 still plays up to its last fragment.

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
    /// A resume's restart waits for the save being written.
    held: bool,
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

impl PowerPlanner {
    /// Every pump tick, with what [`on_event`](Self::on_event) asked for now: a resume's
    /// restart (and its re-probe) is held while `saving`, then goes on the first tick
    /// without. Everything else passes through at once.
    pub fn gate(&mut self, mut actions: Vec<PowerAction>, saving: bool) -> Vec<PowerAction> {
        if actions.contains(&PowerAction::RestartEngine) {
            self.held = true;
            actions.retain(|a| !matches!(a, PowerAction::RestartEngine | PowerAction::Reprobe));
        }
        if self.held && !saving {
            self.held = false;
            actions.extend([PowerAction::RestartEngine, PowerAction::Reprobe]);
        }
        actions
    }

    pub fn held(&self) -> bool {
        self.held
    }
}

/// The power callback's whole job: queue the notice for the pump. Returns whether the pump
/// should be woken (only for the notices it acts on).
pub fn enqueue(pending: &mut Vec<u32>, kind: u32) -> bool {
    pending.push(kind);
    PowerEvent::from_pbt(kind).is_some()
}

/// The suspend/resume registration, the OS handle kept as an integer. Registering again while
/// registered does nothing; unregistering hands the handle back once.
#[derive(Debug, Default)]
pub struct Registration {
    handle: Option<usize>,
}

impl Registration {
    pub const fn new() -> Self {
        Self { handle: None }
    }

    /// Calls `os` only when not registered yet. `Ok(true)`: registered now; `Ok(false)`: already
    /// registered; `Err(code)`: the OS refused (a later call may try again).
    pub fn register(&mut self, os: impl FnOnce() -> Result<usize, u32>) -> Result<bool, u32> {
        if self.handle.is_some() {
            return Ok(false);
        }
        self.handle = Some(os()?);
        Ok(true)
    }

    /// Calls `os` with the handle if registered; false when there was nothing to undo.
    pub fn unregister(&mut self, os: impl FnOnce(usize)) -> bool {
        match self.handle.take() {
            Some(handle) => {
                os(handle);
                true
            }
            None => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{enqueue, PowerAction, PowerEvent, PowerPlanner, Registration, PBT_RESUME_AUTOMATIC, PBT_SUSPEND};

    #[test]
    fn a_second_register_does_not_ask_the_os_again() {
        let mut reg = Registration::new();
        let mut asked = 0;
        assert_eq!(reg.register(|| { asked += 1; Ok(7) }), Ok(true));
        assert_eq!(reg.register(|| { asked += 1; Ok(8) }), Ok(false));
        assert_eq!(asked, 1);
    }

    #[test]
    fn a_refused_register_can_be_tried_again() {
        let mut reg = Registration::new();
        assert_eq!(reg.register(|| Err(5)), Err(5));
        assert_eq!(reg.register(|| Ok(7)), Ok(true));
    }

    #[test]
    fn unregister_hands_the_handle_back_once() {
        let mut reg = Registration::new();
        let mut released = Vec::new();
        assert!(!reg.unregister(|h| released.push(h)), "never registered");
        reg.register(|| Ok(7)).unwrap();
        assert!(reg.unregister(|h| released.push(h)));
        assert!(!reg.unregister(|h| released.push(h)), "unregister after unregister");
        assert_eq!(released, vec![7]);
        assert_eq!(reg.register(|| Ok(9)), Ok(true), "can register again after");
    }

    #[test]
    fn the_callback_queues_every_notice_and_wakes_only_for_ours() {
        let mut pending = Vec::new();
        assert!(enqueue(&mut pending, PBT_SUSPEND));
        assert!(!enqueue(&mut pending, 10), "power source change");
        assert!(enqueue(&mut pending, PBT_RESUME_AUTOMATIC));
        assert_eq!(pending, vec![PBT_SUSPEND, 10, PBT_RESUME_AUTOMATIC]);
    }

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
    fn a_resume_restart_waits_for_the_save_being_written() {
        use PowerAction::{Reprobe, RestartEngine, StopRecording};
        let mut planner = PowerPlanner::default();
        let asked = planner.on_event(PowerEvent::Suspend, true);
        assert_eq!(planner.gate(asked, true), vec![StopRecording], "the suspend is never held");
        let asked = planner.on_event(PowerEvent::Resume, false);
        assert_eq!(planner.gate(asked, true), vec![], "a save is still being written: the restart would abort it");
        assert!(planner.held());
        assert_eq!(planner.gate(Vec::new(), true), vec![], "still writing");
        assert_eq!(planner.gate(Vec::new(), false), vec![RestartEngine, Reprobe], "the save ended: restart now");
        assert_eq!(planner.gate(Vec::new(), false), vec![], "once");
        planner.on_event(PowerEvent::Suspend, false);
        let asked = planner.on_event(PowerEvent::Resume, false);
        assert_eq!(planner.gate(asked, false), vec![RestartEngine, Reprobe], "nothing writing: at once");
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
