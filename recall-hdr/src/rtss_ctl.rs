//! RTSS (RivaTuner Statistics Server) control for the performance-stats
//! island (spec `.planning/ISLAND_SPEC.md`, "RTSS control").
//!
//! RTSS needs administrator rights, so `scripts/install-rtss.ps1` copies the
//! user's RTSS into `%ProgramFiles%\Recall\rtss` (admin-only writable, so the
//! elevated task cannot be hijacked by swapping the exe; measurement-only
//! profiles: no OSD, no limiter, no scanline sync, no Reflex injection) and
//! registers two on-demand scheduled tasks with highest privileges. The
//! controller only runs them (`schtasks /run`, no prompt, no window):
//!
//! - stats on (and at start-up when on): with the private copy and the
//!   [`START_TASK`] task, run it -> `starting` until an RTSS is seen, then
//!   `running`. No copy -> `missing`; a copy but no task -> `needs_setup`;
//!   started but not seen within [`START_TIMEOUT`], or [`MAX_STARTS`] starts
//!   without it staying up -> `needs_setup`.
//! - stats off / controller quit: [`STOP_TASK`], only while Recall owns the
//!   running private copy.
//! - Any other RTSS (the user's own, in Program Files) is never started or
//!   stopped; while one runs, its shared memory feeds the island (`running`).
//!
//! Ownership: only an RTSS.exe under `%ProgramFiles%\Recall\rtss` is Recall's
//! (only the start task launches it), so one a crashed controller left
//! running is adopted and stopped like one started now. An RTSS whose path
//! cannot be read counts as Recall's only after Recall ran the start task. The
//! stop task itself kills only RTSS processes under that folder.

use std::time::{Duration, Instant};

use crate::log;

pub const START_TASK: &str = "Recall RTSS start";
pub const STOP_TASK: &str = "Recall RTSS stop";
/// A started RTSS must show up within this long.
pub const START_TIMEOUT: Duration = Duration::from_secs(15);
/// Start-task runs per stats-on session (an RTSS that keeps exiting).
pub const MAX_STARTS: u32 = 3;
const POLL_STARTING: Duration = Duration::from_millis(500);
const POLL_STEADY: Duration = Duration::from_secs(5);

/// The status `rtss` field.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RtssState {
    Off,
    Starting,
    Running,
    Missing,
    NeedsSetup,
}

impl RtssState {
    pub fn as_str(self) -> &'static str {
        match self {
            RtssState::Off => "off",
            RtssState::Starting => "starting",
            RtssState::Running => "running",
            RtssState::Missing => "missing",
            RtssState::NeedsSetup => "needs_setup",
        }
    }
}

/// The RTSS processes and shared memory seen right now.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Scan {
    /// An RTSS.exe under `%ProgramFiles%\Recall\rtss` (Recall's copy) runs.
    pub ours: bool,
    /// An RTSS.exe elsewhere runs (the user's own).
    pub foreign: bool,
    /// An RTSS.exe whose path could not be read runs.
    pub unknown: bool,
    /// `RTSSSharedMemoryV2` is open with a live `RTSS` signature.
    pub shm: bool,
}

/// Everything [`decide`] looks at.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Facts {
    /// `%ProgramFiles%\Recall\rtss\RTSS.exe` exists.
    pub copy: bool,
    /// The [`START_TASK`] scheduled task exists.
    pub start_task: bool,
    pub scan: Scan,
}

/// What the controller remembers between polls.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Memory {
    /// Recall owns the running private copy (started or adopted).
    pub owned: bool,
    /// When the start task last ran; cleared once RTSS is seen.
    pub started_at: Option<Instant>,
    /// Start-task runs since stats turned on.
    pub starts: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Nothing,
    RunStart,
    RunStop,
}

/// The pure decision: the state to report and the task to run.
pub fn decide(stats_on: bool, f: &Facts, m: &Memory, now: Instant) -> (RtssState, Action) {
    let s = f.scan;
    if !stats_on {
        // `owned` is only ever set for the private copy, and the stop task
        // only kills that copy, so an unreadable RTSS is safe to include.
        let action = if m.owned && (s.ours || s.unknown) { Action::RunStop } else { Action::Nothing };
        return (RtssState::Off, action);
    }
    if s.ours || s.foreign || s.unknown || s.shm {
        return (RtssState::Running, Action::Nothing);
    }
    if !f.copy {
        return (RtssState::Missing, Action::Nothing);
    }
    if !f.start_task {
        return (RtssState::NeedsSetup, Action::Nothing);
    }
    match m.started_at {
        Some(t) if now.duration_since(t) < START_TIMEOUT => (RtssState::Starting, Action::Nothing),
        Some(_) => (RtssState::NeedsSetup, Action::Nothing),
        None if m.starts >= MAX_STARTS => (RtssState::NeedsSetup, Action::Nothing),
        None => (RtssState::Starting, Action::RunStart),
    }
}

/// The machine side: files, the task list, processes (`rtss_win` in the
/// controller, a fake in tests).
pub trait RtssHost {
    fn copy_present(&self) -> bool;
    fn task_exists(&mut self, name: &str) -> bool;
    fn scan(&self) -> Scan;
    fn run_task(&mut self, name: &str) -> Result<(), String>;
}

/// Converges RTSS on `overlay.stats`, polling at the current state's pace.
pub struct RtssCtl {
    host: Box<dyn RtssHost>,
    state: RtssState,
    mem: Memory,
    /// Whether [`START_TASK`] exists, asked once per stats-on session.
    task: Option<bool>,
    /// Next poll; `None` = idle until the next [`RtssCtl::sync`].
    due: Option<Instant>,
}

impl RtssCtl {
    pub fn new(host: Box<dyn RtssHost>) -> Self {
        RtssCtl { host, state: RtssState::Off, mem: Memory::default(), task: None, due: None }
    }

    pub fn state(&self) -> RtssState {
        self.state
    }

    /// Re-check now (start-up, `overlay.stats` changed).
    pub fn sync(&mut self, stats_on: bool, now: Instant) {
        self.due = Some(now);
        self.tick(stats_on, now);
    }

    /// Every pump tick; does work only when a poll is due.
    pub fn tick(&mut self, stats_on: bool, now: Instant) {
        if !self.due.is_some_and(|t| now >= t) {
            return;
        }
        let facts = self.facts(stats_on);
        let (state, action) = decide(stats_on, &facts, &self.mem, now);
        self.apply(action, now);
        if state == RtssState::Running {
            self.mem.started_at = None; // the start worked
        }
        if !stats_on && action == Action::Nothing {
            self.reset();
        }
        if state != self.state {
            log!("rtss: {} -> {}", self.state.as_str(), state.as_str());
            self.state = state;
        }
        self.due = match (stats_on, state) {
            (false, _) => None,
            (true, RtssState::Starting) => Some(now + POLL_STARTING),
            (true, _) => Some(now + POLL_STEADY),
        };
    }

    /// Controller quit: stop the private copy when Recall owns it.
    pub fn shutdown(&mut self) {
        if self.mem.owned {
            self.apply(Action::RunStop, Instant::now());
        }
    }

    fn facts(&mut self, stats_on: bool) -> Facts {
        let scan = self.host.scan();
        if scan.ours || (scan.unknown && self.mem.started_at.is_some()) {
            if !self.mem.owned {
                log!("rtss: Recall's private RTSS is running; Recall stops it when stats turn off");
            }
            self.mem.owned = true;
        }
        let copy = self.host.copy_present();
        let seen = scan.ours || scan.foreign || scan.unknown || scan.shm;
        let start_task = if stats_on && copy && !seen {
            *self.task.get_or_insert_with(|| self.host.task_exists(START_TASK))
        } else {
            self.task.unwrap_or(false)
        };
        Facts { copy, start_task, scan }
    }

    fn apply(&mut self, action: Action, now: Instant) {
        match action {
            Action::Nothing => {}
            Action::RunStart => {
                self.mem.starts += 1;
                self.mem.started_at = Some(now);
                match self.host.run_task(START_TASK) {
                    Ok(()) => log!("rtss: ran \"{START_TASK}\" (start {} of {MAX_STARTS})", self.mem.starts),
                    Err(e) => log!("rtss: \"{START_TASK}\" failed: {e}"),
                }
            }
            Action::RunStop => {
                match self.host.run_task(STOP_TASK) {
                    Ok(()) => log!("rtss: ran \"{STOP_TASK}\""),
                    Err(e) => log!("rtss: \"{STOP_TASK}\" failed: {e}"),
                }
                self.reset();
            }
        }
    }

    /// Stats off: forget ownership, the start budget and the task answer.
    fn reset(&mut self) {
        self.mem = Memory::default();
        self.task = None;
    }
}

#[cfg(test)]
#[path = "rtss_ctl_tests.rs"]
mod tests;
