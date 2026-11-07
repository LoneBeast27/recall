//! Test harness for the transition matrix (`transitions_tests`): the pure pieces the record
//! controller (`record_app::Controller`) runs every pump tick, composed in the same order,
//! with injected time and window state. No Win32, no engine process, no audio.
//!
//! Which live call site each step mirrors:
//! - [`Sim::focus`] / [`Sim::focus_unidentified`]: `record_detect::on_foreground_event`
//!   (`Transitions::candidate` -> `TargetTracker::on_foreground_with`). The transition rules
//!   are the controller's own code (`record_transition::Transitions`, implemented here with
//!   injected state); only the render check is not run: `classify_seen` is `classify_event`
//!   and the games commit like known games / overrides do (`poll_admit` answering `Commit`).
//!   A window's client area is [`Sim::client`].
//! - [`Sim::exit`] / [`Sim::window_gone`]: `record_app::tick` (`target_watch.poll` ->
//!   `on_process_exit`) and `record_detect::check_windows` (`on_window_gone_at`).
//! - [`Sim::tick`]: `record_app::tick`: `poll_tracker` (`feed_drain`, poll) ->
//!   `record_actions::plan_engine` (`set_drain_len(drain_len(..))`, `wanted()`,
//!   `activity.retarget_busy`, `running_drain`, `planner.step`, `note_drain_cut` on Apply,
//!   `closed_game` on a long-recording stop) ->
//!   fresh-engine bookkeeping (`activity.engine_restarted` on the new engine's hello) ->
//!   `activity_tick` (pending pause) -> `record_cmds::announce` (island notices).
//! - [`Sim::save`]: `record_cmds::save_clip`; [`Sim::long_start`]: `engine_command("record_start")`;
//!   [`Sim::pause`] / [`Sim::resume`]: `record_actions::set_paused` / `apply_pause`.
//! - [`Sim::settings_changed`] / [`Sim::restart`] / the deferred retry in [`Sim::tick`]: the
//!   controller's own `record_restart` code (the Sim is a `RestartHost`), as
//!   `request_engine_restart`, `Controller::restart_engine` and `activity_tick` call it.
//!
//! Every `Step::Apply` is one engine (re)spawn or stop (`Supervisor::retarget`), and every
//! spawn starts a new replay ring: "buffer kept" = no `Apply` since the mark.

use std::time::{Duration, Instant};

use crate::config::Config;
use crate::engine_policy::EngineSpec;
use crate::record_engine_ctl::{switch_engine, EngineSwitch};
use crate::record_activity::{EngineActivity, LongRec, PendingPause, Trigger};
use crate::record_config::RecordingCfg;
use crate::record_drain::drain_len;
use crate::record_engine::{capture_refusal, wanted, EnginePlanner, EngineTarget, Step, EXIT_LINGER};
use crate::record_notice::{NoticeTracker, Situation};
use crate::record_restart::{pending_restart_tick, request_settings_restart, restart_engine, RestartHost, RestartReason, Restarted};
use crate::record_target::{classify_event, Target, TargetTracker, START_DEBOUNCE, TARGET_DEBOUNCE};
use crate::record_transition::Transitions;
use crate::watcher::WatchEvent;

/// The pump wakes at least this often (`MAX_WAIT_MS`).
pub const TICK: Duration = Duration::from_millis(50);

pub fn game_ev(stem: &str, pid: u32, hwnd: isize) -> WatchEvent {
    WatchEvent {
        pid,
        exe_stem: stem.into(),
        exe_name: format!("{stem}.exe"),
        title: stem.into(),
        fullscreen: true,
        fullscreen_on_target: true,
        steam_game: false,
        hwnd,
    }
}

/// A non-game foreground (Discord, the desktop, a launcher): `stem` decides how it classifies.
pub fn app_ev(stem: &str, pid: u32, hwnd: isize, fullscreen: bool) -> WatchEvent {
    WatchEvent { fullscreen, fullscreen_on_target: fullscreen, ..game_ev(stem, pid, hwnd) }
}

/// A windowed exe in a Steam library (a launcher, a crash handler shipped with the game).
pub fn steam_ev(stem: &str, pid: u32, hwnd: isize) -> WatchEvent {
    WatchEvent { steam_game: true, ..app_ev(stem, pid, hwnd, false) }
}

pub struct Sim {
    pub t0: Instant,
    pub now: Instant,
    cfg: Config,
    rec: RecordingCfg,
    pub tracker: TargetTracker,
    pub planner: EnginePlanner,
    pub activity: EngineActivity,
    notices: NoticeTracker,
    pub paused: bool,
    pending_pause: Option<PendingPause>,
    foreground: Option<WatchEvent>,
    /// Buffered seconds the engine reports (`engine_view().buffer_secs`).
    pub buffered: Option<f32>,
    /// Every applied engine target, with when.
    pub applied: Vec<(Instant, EngineTarget)>,
    /// Engine command lines sent, with the engine target that received them.
    pub sent: Vec<(String, EngineTarget)>,
    /// Island notices announced (with `overlay.enabled` on).
    pub notices_said: Vec<Situation>,
    /// Ticks the planner answered `Step::Held`.
    pub held_ticks: u32,
    /// The client area every focused window reports (`watcher::client_size`).
    pub client: (i32, i32),
    /// The pinned window (`Controller::pinned`).
    pub pinned: Option<Target>,
    /// Shortcut-style island notices (title, detail): `Transitions::island`.
    pub island_said: Vec<(String, String)>,
    /// A restart waits for a save / drain (`Controller::pending_restart`).
    pub pending_restart: Option<RestartReason>,
    /// Controller log lines the shared restart code wrote (`log!`).
    pub notes: Vec<String>,
    /// The supervisor calls the restart code made (`RestartHost::restart_apply`).
    pub switches: SwitchLog,
}

/// A supervisor call (`record_engine_ctl::switch_engine`); `stop`: to Off (no spec).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Switch {
    Retarget { stop: bool },
    Restart { stop: bool, planned: bool },
}

/// Records the calls in place of the supervisor.
#[derive(Debug, Default)]
pub struct SwitchLog(pub std::cell::RefCell<Vec<Switch>>);

impl EngineSwitch for SwitchLog {
    fn switch_to(&self, spec: Option<EngineSpec>) {
        self.0.borrow_mut().push(Switch::Retarget { stop: spec.is_none() });
    }
    fn restart_to(&self, spec: Option<EngineSpec>, planned: bool) {
        self.0.borrow_mut().push(Switch::Restart { stop: spec.is_none(), planned });
    }
}

impl SwitchLog {
    pub fn last(&self) -> Option<Switch> {
        self.0.borrow().last().copied()
    }
    pub fn count(&self) -> usize {
        self.0.borrow().len()
    }
}

impl Sim {
    pub fn new() -> Sim {
        let t0 = Instant::now();
        Sim {
            t0,
            now: t0,
            cfg: Config::default(),
            rec: RecordingCfg::default(),
            tracker: TargetTracker::new(TARGET_DEBOUNCE, START_DEBOUNCE),
            planner: EnginePlanner::new(EXIT_LINGER),
            activity: EngineActivity::default(),
            notices: NoticeTracker::new(),
            paused: false,
            pending_pause: None,
            foreground: None,
            buffered: Some(120.0),
            applied: Vec::new(),
            sent: Vec::new(),
            notices_said: Vec::new(),
            held_ticks: 0,
            client: (1920, 1080),
            pinned: None,
            island_said: Vec::new(),
            pending_restart: None,
            notes: Vec::new(),
            switches: SwitchLog::default(),
        }
    }

    pub fn ms(&self, ms: u64) -> Instant {
        self.t0 + Duration::from_millis(ms)
    }

    /// Run pump ticks every [`TICK`] up to `ms` (inclusive).
    pub fn run_to(&mut self, ms: u64) {
        let end = self.ms(ms);
        while self.now + TICK <= end {
            self.now += TICK;
            self.tick();
        }
        if self.now < end {
            self.now = end;
            self.tick();
        }
    }

    pub fn focus(&mut self, ev: WatchEvent) {
        let now = self.now;
        let (candidate, traits) = self.candidate(&ev, now);
        self.foreground = Some(ev);
        self.tracker.on_foreground_with(candidate, traits, now);
    }

    /// A foreground the watcher cannot identify (OpenProcess fails): the hook and the
    /// 1 s probe feed it as `WatchEvent::unknown` (`watcher::event_or_unknown`).
    pub fn focus_unidentified(&mut self) {
        self.focus(WatchEvent::unknown(999, 0x999));
    }

    pub fn exit(&mut self, pid: u32) {
        self.tracker.on_process_exit(pid);
    }

    pub fn window_gone(&mut self, hwnd: isize) {
        self.tracker.on_window_gone_at(hwnd, self.now);
    }

    pub fn wanted(&self) -> EngineTarget {
        wanted(self.tracker.current(), self.pinned.as_ref(), &self.rec, self.paused)
    }

    pub fn engine(&self) -> &EngineTarget {
        self.planner.applied()
    }

    pub fn tick(&mut self) {
        let now = self.now;
        if self.pending_pause.is_some_and(|p| p.ready(&self.activity, now)) {
            self.apply_pause(true);
        }
        self.feed_drain(now);
        self.tracker.poll(now);
        self.planner.set_drain_len(drain_len(self.buffered, self.rec.buffer_secs));
        let busy = self.activity.retarget_busy(now);
        let drained = self.running_drain(now);
        match self.planner.step(&self.wanted(), now, busy) {
            Step::Apply(target) => {
                self.note_drain_cut(drained.as_ref(), &target);
                self.applied_engine(target)
            }
            Step::Held => self.held_ticks += 1,
            // `plan_engine`: one record_stop per recording, the retarget follows its stop.
            Step::StopLongRec { game_closed } => {
                let game_closed = self.closed_game(game_closed);
                if self.activity.auto_stop(now, game_closed) {
                    self.sent.push(("record_stop".into(), self.engine().clone()));
                }
            }
            Step::Keep => {}
        }
        pending_restart_tick(self); // `activity_tick`
        self.announce();
    }

    fn applied_engine(&mut self, target: EngineTarget) {
        if target != EngineTarget::Off {
            // The new engine's hello (`record_app::tick`): nothing of the old one is in flight.
            self.activity.engine_restarted();
        }
        // `apply_engine_target`: any (re)start or stop picks up the current settings.
        self.pending_restart = None;
        self.applied.push((self.now, target));
    }

    /// An engine setting changed in the app: `request_engine_restart` (the real code).
    pub fn settings_changed(&mut self) {
        request_settings_restart(self);
    }

    /// A restart that is no game switch (resume, recovery): `Controller::restart_engine`.
    pub fn restart(&mut self, reason: RestartReason) -> Restarted {
        restart_engine(self, reason)
    }

    /// `recording.record_desktop_when_idle`.
    pub fn set_desktop_when_idle(&mut self, on: bool) {
        self.rec.record_desktop_when_idle = on;
    }

    /// `record_cmds::announce` with a healthy, fresh engine (`session_state` "recording").
    fn announce(&mut self) {
        let state = if self.paused {
            "paused"
        } else if self.planner.drain().is_some() {
            "draining" // `record_status::state_with_drain`
        } else if self.wanted() == EngineTarget::Off {
            "waiting"
        } else {
            "recording"
        };
        let next = match (state, self.planner.drain()) {
            ("draining", Some((_, _, len))) => Some(Situation::Draining { secs: len.as_secs_f32().round() as u64 }),
            _ => Situation::of(state, &self.wanted(), false),
        };
        if let Some(n) = self.notices.observe(next, self.now) {
            self.notices_said.push(n);
        }
    }

    fn engine_running(&self) -> bool {
        *self.planner.applied() != EngineTarget::Off
    }

    /// `save_clip`: `true` when the engine was asked.
    pub fn save(&mut self) -> bool {
        if capture_refusal(self.paused, self.engine_running(), &self.wanted()).is_some() {
            return false;
        }
        self.activity.save_sent(self.now);
        self.sent.push(("save_clip".into(), self.engine().clone()));
        true
    }

    /// The engine reports a clip landed.
    pub fn saved(&mut self) {
        self.activity.on_event(&serde_json::json!({"type": "saved", "path": "c.mp4"}), 0.0);
    }

    /// The engine reports the long recording stopped: what `drain_engine_events` acts on.
    pub fn long_stopped(&mut self) -> Option<Trigger> {
        self.activity.on_event(&serde_json::json!({"type": "recording_stopped", "path": "r.mp4", "seconds": 9.0, "bytes": 1}), 0.0)
    }

    /// Alt+L on an idle engine, and the engine reporting it started.
    pub fn long_start(&mut self) {
        self.activity.record_requested(self.now);
        self.sent.push(("record_start".into(), self.engine().clone()));
        self.activity.on_event(&serde_json::json!({"type": "recording_started", "path": "r.mp4"}), 1.0);
    }

    pub fn long_running(&self) -> bool {
        self.activity.long != LongRec::Idle
    }

    pub fn sent_stop(&self) -> bool {
        self.sent.iter().any(|(c, _)| c == "record_stop")
    }

    pub fn pause(&mut self) {
        if !self.paused && self.engine_running() {
            if let Some(wait) = PendingPause::needed(&self.activity, self.now) {
                self.sent.push(("record_stop".into(), self.engine().clone()));
                self.pending_pause = Some(wait);
                return;
            }
        }
        self.apply_pause(true);
    }

    /// Resume re-reads the foreground (`probe_foreground` with `last_fed` cleared).
    pub fn resume(&mut self) {
        self.pending_pause = None;
        self.paused = false;
        if let Some(ev) = self.foreground.clone() {
            self.focus(ev);
        }
        self.tick();
    }

    fn apply_pause(&mut self, paused: bool) {
        self.pending_pause = None;
        self.paused = paused;
        if self.planner.pause() {
            self.applied_engine(EngineTarget::Off); // `apply_engine_target(Off)`
        }
    }

    /// Engine spawns (not stops) since `since`.
    pub fn spawns_since(&self, since: Instant) -> usize {
        self.applied.iter().filter(|(at, t)| *at > since && *t != EngineTarget::Off).count()
    }

    /// No engine (re)spawn or stop since `since`: the replay ring is the same one.
    pub fn ring_kept_since(&self, since: Instant) -> bool {
        self.applied.iter().all(|(at, _)| *at <= since)
    }

    pub fn current(&self) -> Option<&Target> {
        self.tracker.current()
    }

    pub fn recording_of(&self, pid: u32) -> bool {
        matches!(self.engine(), EngineTarget::Window(t) if t.pid == pid)
    }

    /// When the engine was first applied to the game with `pid`.
    pub fn applied_at(&self, pid: u32) -> Option<Instant> {
        self.applied.iter().find(|(_, t)| matches!(t, EngineTarget::Window(w) if w.pid == pid)).map(|(at, _)| *at)
    }

    pub fn game_notices(&self, name: &str) -> usize {
        self.notices_said.iter().filter(|n| **n == Situation::Game(name.into())).count()
    }
}

impl Transitions for Sim {
    fn cfg(&self) -> &Config {
        &self.cfg
    }
    fn tracker(&self) -> &TargetTracker {
        &self.tracker
    }
    fn tracker_mut(&mut self) -> &mut TargetTracker {
        &mut self.tracker
    }
    fn planner(&self) -> &EnginePlanner {
        &self.planner
    }
    fn pinned(&self) -> Option<&Target> {
        self.pinned.as_ref()
    }
    fn is_paused(&self) -> bool {
        self.paused
    }
    fn classify_seen(&mut self, ev: &WatchEvent, _now: Instant) -> Option<Target> {
        classify_event(ev, &self.cfg)
    }
    fn client_of(&self, _hwnd: isize) -> (i32, i32) {
        self.client
    }
    fn island(&mut self, title: &str, detail: &str) {
        self.island_said.push((title.into(), detail.into()));
    }
}

/// The Sim as the controller's restart host: the same planner, activity and flag fields.
impl RestartHost for Sim {
    fn restart_planner(&mut self) -> &mut EnginePlanner {
        &mut self.planner
    }
    fn restart_busy(&mut self) -> bool {
        self.activity.busy(self.now)
    }
    fn restart_wanted(&self) -> EngineTarget {
        self.wanted()
    }
    fn restart_pending(&mut self) -> &mut Option<RestartReason> {
        &mut self.pending_restart
    }
    fn restart_apply(&mut self, target: &EngineTarget, reason: RestartReason) {
        // As `Controller::restart_apply` -> `apply_engine_target_as` -> `switch_engine`.
        let spec = (*target != EngineTarget::Off).then(EngineSpec::default);
        switch_engine(&self.switches, spec, reason.refusal());
        self.applied_engine(target.clone());
    }
    fn restart_notice(&mut self, title: &str, detail: &str) {
        self.island_said.push((title.into(), detail.into()));
    }
    fn restart_log(&mut self, line: &str) {
        self.notes.push(line.into());
    }
}
