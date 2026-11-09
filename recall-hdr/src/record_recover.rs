//! Engine restart after a lasting encoder failure: the last resort of encoder recovery.
//!
//! The engine goes first (`recall/src/encoder/reopen.rs`): it rebuilds the encoder in
//! place keeping the replay buffer, asks for a capture restart, falls back to CPU copies,
//! and meanwhile reports `encoder: recovering` (session `recovering`). It reports
//! `failed` only once that plan is spent (5 real failed reopens; time without frames counts
//! for nothing).
//! The controller is keyed off that engine state, not off a shorter timer.
//!
//! When the session sits in `error` from `encoder: failed` for longer than
//! [`ERROR_GRACE`], the engine is restarted as an INTENDED restart (a retarget to the same
//! target: the supervisor never counts it toward give-up). These restarts have their own
//! backoff ([`RESTART_GAPS`]); a minute of recording forgets it. The island says
//! "Recording again" when recording comes back (`record_notice`).
//!
//! A "recovering" engine whose recovery stopped moving counts as `encoder: failed` here
//! (`record_recover_stale`), but only while the game can be seen.

use std::time::{Duration, Instant};

use crate::log;
use crate::record_recover_stale::{EngineWatch, Verdict};
use crate::record_recovery_act::EarlyStep;
use crate::record_status::session_state;
use crate::supervisor::EngineHealth;

/// The session must sit in `error` from `encoder: failed` longer than this.
pub const ERROR_GRACE: Duration = Duration::from_secs(15);
/// Least time between two of these restarts: 30 s, 60 s, 2 min, then every 5 min.
pub const RESTART_GAPS: [Duration; 4] =
    [Duration::from_secs(30), Duration::from_secs(60), Duration::from_secs(120), Duration::from_secs(300)];
/// Recording this long after a restart forgets the backoff.
pub const STABLE_AFTER: Duration = Duration::from_secs(60);
/// A save in flight that shows no progress holds a hang restart this long at most.
pub const HANG_SAVE_HOLD: Duration = Duration::from_secs(15);
/// A save that keeps growing holds a hang restart this long at most.
pub const HANG_GROW_HOLD: Duration = Duration::from_secs(300);
/// Why a recording ends early when the engine restarts under it (`record_fail`).
pub const ENDED_EARLY_REASON: &str = "the encoder isn't responding";

/// The session as this rule sees it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Seen {
    /// `error` because the engine reported `encoder: failed` (not a supervisor give-up).
    EncoderFailed,
    Recording,
    /// Engine starting or restarting.
    Starting,
    /// Waiting, paused, draining or given up: nothing to recover.
    Idle,
}

/// What holds a restart back now (`encoder_recover_tick`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Hold {
    /// A save or a screenshot in flight.
    pub saving: bool,
    /// A long recording starting or running.
    pub long_rec: bool,
    /// A capture-stuck restart in flight (never stack a second one).
    pub stuck: bool,
    /// The save in flight still makes progress (`EngineActivity::save_growing`): the
    /// saver is a stream copy on its own thread, so a hung encoder does not stop it.
    pub growing: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Recovery {
    /// The engine hung again right after its hang restart: one notice, no more restarts
    /// until the game or the settings change.
    GaveUp,
    Nothing,
    /// Restart the engine now (an intended restart).
    Restart,
    /// Recording came back after a restart.
    Recovered,
}

impl Seen {
    /// From a `record_status::session_state` and the supervisor's health.
    pub fn of(state: &str, health: EngineHealth) -> Seen {
        match state {
            "error" if health != EngineHealth::GivenUp => Seen::EncoderFailed,
            // CPU copies still record: nothing to restart.
            "recording" | "degraded" => Seen::Recording,
            // The engine's own recovery (encoder rebuild, capture restart, CPU copies) goes
            // first; it says "failed" when that is spent. The restart here is the last resort.
            "starting" | "recovering" => Seen::Starting,
            _ => Seen::Idle,
        }
    }
}

#[derive(Debug, Default)]
pub struct EncoderRecovery {
    /// Since when the error has lasted without a break.
    failed_since: Option<Instant>,
    /// Restarts in this episode and when the last one was asked.
    restarts: u32,
    last_restart: Option<Instant>,
    /// A restart was asked and recording has not come back yet.
    awaiting: bool,
    /// Recording since (after a restart): a stable minute ends the episode.
    healthy_since: Option<Instant>,
    /// A hung engine, or a recovery getting nowhere (`record_recover_stale`). Kept across
    /// the stable-minute reset: a hang gets one restart, not one per minute.
    watch: EngineWatch,
    /// A hang verdict held back by a save in flight, since.
    hang_held_since: Option<Instant>,
    /// Hung again right after its hang restart: no restarts until the game or settings change.
    gave_up: bool,
    /// The target the session records (a new one ends a give-up).
    target: String,
}

impl EncoderRecovery {
    /// Feed the session every pump tick; `busy` = a save or the long recording in flight.
    pub fn observe(&mut self, now: Instant, seen: Seen, busy: bool) -> Recovery {
        if seen != Seen::EncoderFailed {
            self.failed_since = None;
        }
        match seen {
            Seen::EncoderFailed => {
                self.healthy_since = None;
                let since = *self.failed_since.get_or_insert(now);
                if now.saturating_duration_since(since) <= ERROR_GRACE || busy || !self.gap_passed(now) {
                    return Recovery::Nothing;
                }
                self.restarts = self.restarts.saturating_add(1);
                self.last_restart = Some(now);
                self.failed_since = None;
                self.awaiting = true;
                Recovery::Restart
            }
            Seen::Recording => self.recording(now),
            Seen::Starting => Recovery::Nothing,
            Seen::Idle => {
                let target = std::mem::take(&mut self.target);
                *self = EncoderRecovery { target, ..Default::default() };
                Recovery::Nothing
            }
        }
    }

    /// An error is being timed, or a restart from here was asked under a minute ago and
    /// recording has not come back (`record_capture_watch` stands aside meanwhile).
    pub fn pending(&self, now: Instant) -> bool {
        self.failed_since.is_some() || (self.awaiting && self.last_restart.is_some_and(|t| now.saturating_duration_since(t) < STABLE_AFTER))
    }

    /// One pump tick from the session: `progress` = the encoder heartbeat (or the status
    /// line count on an engine without one), `shown` = the game can be seen. A hung engine
    /// gets one restart at once; a recovery past its bound is fed as [`Seen::EncoderFailed`].
    /// `busy`: anything that holds a restart (all as a capture-stuck restart would).
    pub fn step(&mut self, now: Instant, state: &str, health: EngineHealth, progress: u64, shown: bool, busy: bool) -> Recovery {
        let target = self.target.clone();
        self.step_held(now, state, health, progress, false, shown, Hold { stuck: busy, ..Hold::default() }, &target)
    }

    /// [`Self::step`] with what holds a restart told apart. A hang verdict and the 90 s
    /// bound are not held by a long recording (a hung encoder sends it nothing, so it would
    /// wait forever; the restart ends it early, `restart_lines`); a save in flight holds a
    /// hang restart [`HANG_SAVE_HOLD`] at most, [`HANG_GROW_HOLD`] while it still grows; a capture-stuck restart in flight holds
    /// everything. `opening`: the engine says an NVENC open is in progress. `target`: what
    /// is recorded; a new one, like new settings, ends a give-up.
    #[allow(clippy::too_many_arguments)]
    pub fn step_held(&mut self, now: Instant, state: &str, health: EngineHealth, progress: u64, opening: bool, shown: bool, hold: Hold, target: &str) -> Recovery {
        if self.target != target {
            self.target = target.to_string();
            self.settings_changed();
        }
        let verdict = self.watch.observe(state, progress, opening, shown, now);
        if self.gave_up {
            // A healthy minute ends a give-up too: a later real `failed` in the same game
            // still gets its last-resort restart.
            if !self.watch.healthy(now) {
                return Recovery::Nothing;
            }
            self.gave_up = false;
        }
        if verdict != Some(Verdict::Hung) {
            self.hang_held_since = None;
        }
        match verdict {
            Some(Verdict::Hung) => self.hang_restart(now, hold),
            Some(Verdict::HungAgain) => {
                self.gave_up = true;
                Recovery::GaveUp
            }
            Some(Verdict::Failed) => self.observe(now, Seen::EncoderFailed, hold.saving || hold.stuck),
            None => self.observe(now, Seen::of(state, health), hold.saving || hold.long_rec || hold.stuck),
        }
    }

    fn hang_restart(&mut self, now: Instant, hold: Hold) -> Recovery {
        let held = *self.hang_held_since.get_or_insert(now);
        let waited = now.saturating_duration_since(held);
        let save_holds = hold.saving && (waited < HANG_SAVE_HOLD || (hold.growing && waited < HANG_GROW_HOLD));
        if hold.stuck || save_holds {
            return Recovery::Nothing;
        }
        self.hang_held_since = None;
        self.watch.hang_restart_done();
        self.awaiting = true;
        self.last_restart = Some(now);
        Recovery::Restart
    }

    /// New engine settings: a give-up ends, the next hang gets its restart again.
    pub fn settings_changed(&mut self) {
        self.gave_up = false;
        self.watch.forget_hangs();
    }

    fn recording(&mut self, now: Instant) -> Recovery {
        if self.restarts == 0 {
            return Recovery::Nothing;
        }
        let since = *self.healthy_since.get_or_insert(now);
        if now.saturating_duration_since(since) >= STABLE_AFTER {
            let (watch, target, gave_up) = (std::mem::take(&mut self.watch), std::mem::take(&mut self.target), self.gave_up);
            *self = EncoderRecovery { watch, target, gave_up, ..Default::default() };
            return Recovery::Nothing;
        }
        if std::mem::take(&mut self.awaiting) {
            return Recovery::Recovered;
        }
        Recovery::Nothing
    }

    /// The backoff since the last restart has run out (no restart yet: it has).
    fn gap_passed(&self, now: Instant) -> bool {
        let gap = RESTART_GAPS[(self.restarts.max(1) as usize - 1).min(RESTART_GAPS.len() - 1)];
        self.last_restart.is_none_or(|at| now.saturating_duration_since(at) >= gap)
    }
}

/// The engine lines sent before an engine restart from here: a running long recording is
/// ended early first (`record_fail`; the recording thread finishes it whatever the encoder
/// does, every part kept, and the app says "Recording ended early"). The quit that follows
/// cannot finalize what a hung encoder holds; a `.part` a kill leaves is still recovered on
/// the next start.
pub fn restart_lines(long_rec: bool) -> Vec<String> {
    if !long_rec {
        return Vec::new();
    }
    vec![serde_json::json!({"cmd": "record_fail", "reason": ENDED_EARLY_REASON}).to_string()]
}

/// How long the restart waits after `record_fail` for `recording_failed` (the part's
/// finish on a slow HDD). Then it quits anyway; the next start recovers the `.part`.
pub const END_EARLY_WAIT: Duration = Duration::from_secs(20);

/// A restart waiting for the long recording to end early (a state, polled every pump
/// tick; never a sleep on the pump).
/// What a restart waiting for the recording to end does this tick.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EndDecision {
    Wait,
    RestartNow,
    /// A new engine came up meanwhile (the supervisor restarted a crash): nothing to restart.
    Cancel,
}

#[derive(Debug, Clone, Copy)]
pub struct EndingEarly {
    pub until: Instant,
    hellos: u32,
}

impl EndingEarly {
    /// `hellos`: engine hellos so far (`Supervisor::hello_count`): a new one is a new engine.
    pub fn new(now: Instant, hellos: u32) -> Self {
        EndingEarly { until: now + END_EARLY_WAIT, hellos }
    }

    /// Quit now: the recording ended (`recording`: it still runs), or the wait is over.
    pub fn restart_now(&self, now: Instant, recording: bool) -> bool {
        !recording || now >= self.until
    }

    /// The wait's decision this tick; `hellos`: engine hellos so far. A new hello is a new
    /// engine (the supervisor restarted a crash meanwhile): cancel, never restart it again.
    pub fn decide(&self, now: Instant, recording: bool, hellos: u32) -> EndDecision {
        if hellos != self.hellos {
            EndDecision::Cancel
        } else if self.restart_now(now, recording) {
            EndDecision::RestartNow
        } else {
            EndDecision::Wait
        }
    }
}

/// The one notice when a hung engine gives up: "Recording stopped".
pub fn gave_up_notice() -> String {
    serde_json::json!({"type": "notice", "kind": "encoder", "title": "Recording stopped", "detail": "The encoder isn't responding"}).to_string()
}

impl crate::record_app::Controller {
    /// Every pump tick: restart the engine when the encoder error has lasted (intended).
    pub(crate) fn encoder_recover_tick(&mut self, now: Instant) {
        // A restart waiting for the long recording to end early (`record_fail` sent).
        if let Some(wait) = self.ending_early {
            let recording = self.activity.is_recording();
            let decision = wait.decide(now, recording, self.supervisor.hello_count());
            if decision == EndDecision::RestartNow && recording {
                log!("record: the long recording did not end within {} s; restarting anyway (the next start recovers its .part)", END_EARLY_WAIT.as_secs());
            }
            match crate::record_recovery_act::ending_early_step(self, decision) {
                EarlyStep::Wait => return,
                EarlyStep::Cancelled => log!("record: a new engine started while the long recording ended early; no restart"),
                EarlyStep::Restarted => {}
            }
            self.ending_early = None;
            return;
        }
        let health = self.supervisor.health();
        let view = self.supervisor.engine_view();
        let fresh = self.supervisor.hello_count() > self.hello_at_retarget;
        let wanted = self.wanted();
        let state = session_state(&wanted, health, view.encoder.as_deref(), fresh, self.paused);
        let watched = matches!(state, "recovering" | "recording" | "degraded");
        let shown = match &wanted {
            crate::record_engine::EngineTarget::Window(t) if watched => crate::record_screen::sight(t).shown(),
            _ => true,
        };
        // The encoder heartbeat when the engine sends one, else its status line count.
        let progress = view.heartbeat.filter(|h| *h > 0).unwrap_or(view.statuses);
        let saving = self.activity.retarget_busy(now).saving;
        // A capture-stuck restart in flight: never stack a second one on it.
        let hold = Hold { saving, long_rec: self.activity.busy(now) && !saving, stuck: self.capture_stuck.pending(now), growing: self.activity.save_growing(now) };
        let target = match &wanted {
            crate::record_engine::EngineTarget::Window(t) => format!("{}:{}", t.exe, t.pid),
            crate::record_engine::EngineTarget::Desktop => "desktop".into(),
            crate::record_engine::EngineTarget::Off => String::new(),
        };
        match self.recovery.step_held(now, state, health, progress, view.opening, shown, hold, &target) {
            Recovery::Restart => {
                log!("record: the encoder failed, its recovery ran past its bound, or the engine hung; restarting the engine (intended)");
                let lines = crate::record_recovery_act::recovery_restart(self, self.activity.is_recording());
                if lines.is_empty() {
                    return; // restarted
                }
                // The quit waits (up to 20 s) for the recording thread to finish the part.
                log!("record: ending the long recording early; the restart follows once it reports");
                for line in lines {
                    // Not a user action: logged; the 20 s wait bounds it (then the restart).
                    if !crate::record_dispatch::automatic_effects(self.supervisor.send_engine_line(&line)).is_empty() {
                        log!("record: record_fail not sent ({}); the restart follows within {} s", crate::record_dispatch::NOT_RESPONDING, END_EARLY_WAIT.as_secs());
                    }
                }
                self.ending_early = Some(EndingEarly::new(now, self.supervisor.hello_count()));
            }
            Recovery::Recovered => log!("record: recording again after the engine restart"),
            Recovery::GaveUp => {
                log!("record: the engine hung again right after its restart; no more restarts until the game or the settings change");
                if self.cfg.overlay.shows_notices() {
                    self.hub.event("session", &gave_up_notice());
                }
                crate::cues::failed();
            }
            Recovery::Nothing => {}
        }
    }
}

#[cfg(test)]
#[path = "record_recover_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "record_recover_hang_tests.rs"]
mod hang_tests;

#[cfg(test)]
#[path = "record_recover_hold_tests.rs"]
mod hold_tests;
