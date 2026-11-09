//! Test harness for the transition matrix (`transitions_tests`): the pure pieces the record
//! controller (`record_app::Controller`) runs every pump tick, composed in the same order,
//! with injected time and window state. No Win32, no engine process, no audio.
//!
//! Which live call site each step mirrors:
//! - [`Sim::focus`] / [`Sim::focus_unidentified`]: `record_detect::on_foreground_event`
//!   (`Transitions::candidate` -> `TargetTracker::on_foreground_with`). The transition rules
//!   are the controller's own code (`record_transition::Transitions`, implemented here with
//!   injected state); the render check itself is not run: `classify_seen` applies the proof
//!   rule (`record_hosted::check_proof`, with [`Sim::can_check`]) and a checked candidate
//!   commits on a "game" verdict unless [`Sim::not_games`] names it (R8-detect-6).
//!   [`Sim::set_config`]: `recheck_registration`.
//!   A window's client area is [`Sim::client`]. [`Sim::seen`]: `on_foreground_seen` (the 1 s
//!   probe), fed only when `record_detect::fed_key` changed.
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
use crate::record_activity::{EngineActivity, LongRec, PendingPause, Trigger};
use crate::record_config::RecordingCfg;
use crate::record_detect::{fed_key, FedKey};
use crate::record_engine::{capture_refusal, wanted, EnginePlanner, EngineTarget, EXIT_LINGER};
use crate::record_notice::{NoticeTracker, Situation};
use crate::record_restart::{pending_restart_tick, request_settings_restart, restart_engine, restart_for_recovery, RestartReason, Restarted};
use crate::record_target::{Target, TargetTracker, START_DEBOUNCE, TARGET_DEBOUNCE};
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
        launcher_game: false,
        store_game: false,
        hosted: false,
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
    /// `Controller::retarget_held`, and the releases the planner settled (`release_settled`).
    pub retarget_held: bool,
    pub settles: u32,
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
    /// The last foreground fed to the tracker (`Controller::last_fed`).
    last_fed: Option<FedKey>,
    /// The render check can run (trace rights): `record_hosted::check_proof`'s `can_check`.
    pub can_check: bool,
    /// Stems whose render check says "not a game" (every other checked window is a game).
    pub not_games: Vec<String>,
    /// `watcher::probe_window` of the session's window fails (`recheck_registration`).
    pub probe_fails: bool,
    /// The window the render check is asked about (`RenderCheck::wanted`).
    checked: Option<(u32, isize)>,
    /// The engine says `stop_capture` / `drain_heartbeat` in its hello (older ones do not).
    pub engine_releases: bool,
    pub engine_heartbeats: bool,
    /// Privacy stops that released the engine (`release_engine_to`), with the next target:
    /// capture stopped, saves in flight finish, then that target starts.
    pub released: Vec<(Instant, EngineTarget)>,
    /// Time the simulated PC slept (`Sim::sleep_for`): it moves `now`, never the awake clock.
    slept: Duration,
    /// Sleep and resume (`Controller::power`), the notices queued for the next `power_tick`
    /// (`Sim::power_notice`), and the `save_failed` lines the controller put on the hub.
    pub power: crate::record_power::PowerPlanner,
    pub power_notices: Vec<u32>,
    pub hub_events: Vec<String>,
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
            retarget_held: false,
            settles: 0,
            client: (1920, 1080),
            pinned: None,
            island_said: Vec::new(),
            pending_restart: None,
            notes: Vec::new(),
            switches: SwitchLog::default(),
            last_fed: None,
            can_check: true,
            not_games: Vec::new(),
            probe_fails: false,
            checked: None,
            engine_releases: true,
            engine_heartbeats: true,
            released: Vec::new(),
            slept: Duration::ZERO,
            power: Default::default(),
            power_notices: Vec::new(),
            hub_events: Vec::new(),
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
        self.end_if_windowed(&ev);
        let now = self.now;
        let (candidate, traits) = self.candidate(&ev, now);
        self.last_fed = Some(fed_key(ev.hwnd, candidate.is_some(), traits));
        self.foreground = Some(ev);
        self.tracker.on_foreground_with(candidate, traits, now);
    }

    /// A foreground from the 1 s probe or the follow probe:
    /// `record_detect::on_foreground_seen`, fed only when its `fed_key` changed.
    pub fn seen(&mut self, ev: WatchEvent) {
        self.end_if_windowed(&ev);
        let now = self.now;
        let (candidate, traits) = self.candidate(&ev, now);
        let key = fed_key(ev.hwnd, candidate.is_some(), traits);
        self.foreground = Some(ev);
        if self.last_fed != Some(key) {
            self.last_fed = Some(key);
            self.tracker.on_foreground_with(candidate, traits, now);
        }
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
        let awake = self.awake();
        if self.pending_pause.map(|p| p.ready(&mut self.activity, awake)) == Some(true) {
            self.apply_pause(true);
        }
        self.feed_drain(now);
        self.poll_with_verdicts(now);
        crate::record_plan::PlanHost::plan_tick(self, now); // `plan_engine`
        pending_restart_tick(self); // `activity_tick`
        self.announce();
    }

    fn applied_engine(&mut self, target: EngineTarget) {
        // The new engine's hello (`record_app::tick`): nothing of the old one is in flight. A
        // released engine drains first (the slot admits one recorder): its saves stay counted
        // until it exits (`Sim::engine_exited`), as on the controller.
        if target != EngineTarget::Off && self.activity.release().is_none() {
            let exited = serde_json::json!({"type": crate::supervisor::ENGINE_EXITED});
            let _ = self.activity.on_event(&exited, 0.0); // the old engine's exit is read first
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

    /// A recovery restart: `Controller::restart_for_recovery` (the real helper).
    pub fn recover(&mut self) -> Restarted {
        restart_for_recovery(self)
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
        if capture_refusal(self.paused, self.engine_running(), &self.wanted()).is_some() || self.planner.privacy_command_refusal("save_clip", self.activity.is_recording()).is_some() {
            return false;
        }
        let now = self.awake();
        self.activity.save_sent(now);
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
        let now = self.awake();
        self.activity.record_requested(now);
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
            let now = self.awake();
            if let Some(wait) = PendingPause::needed(&mut self.activity, now) {
                if self.activity.long != LongRec::Idle {
                    self.sent.push(("record_stop".into(), self.engine().clone()));
                }
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
    fn planner_mut(&mut self) -> &mut EnginePlanner {
        &mut self.planner
    }
    fn wanted_now(&self) -> EngineTarget {
        self.wanted()
    }
    fn apply_now(&mut self, target: &EngineTarget) {
        self.applied_engine(target.clone());
    }
    fn release_now(&mut self, target: &EngineTarget) -> bool {
        if self.engine_releases {
            self.pending_restart = None; // `apply_engine_target_how`
            self.released.push((self.now, target.clone()));
        }
        self.engine_releases
    }
    fn saving(&mut self) -> bool {
        self.activity.save_in_flight() // as `Controller::saving`
    }
    fn activity_mut(&mut self) -> &mut EngineActivity {
        &mut self.activity
    }
    fn engine_heartbeats(&self) -> bool {
        self.engine_heartbeats
    }
    fn awake_now(&self) -> crate::awake_clock::Awake {
        self.awake()
    }
    fn now(&self) -> Instant {
        self.now
    }
    fn pinned(&self) -> Option<&Target> {
        self.pinned.as_ref()
    }
    fn is_paused(&self) -> bool {
        self.paused
    }
    fn classify_seen(&mut self, ev: &WatchEvent, _now: Instant) -> Option<Target> {
        self.classify_with_proof(ev)
    }
    fn client_of(&self, _hwnd: isize) -> (i32, i32) {
        self.client
    }
    fn island(&mut self, title: &str, detail: &str) {
        self.island_said.push((title.into(), detail.into()));
    }
}

/// The Sim's awake-only clock and sleep, and the ring checks, split out for file size.
#[path = "transitions_sim_clock.rs"]
mod clock;

/// The Sim as the controller's restart and power hosts (`record_restart`, `record_power_host`).
#[path = "transitions_sim_hosts.rs"]
mod hosts;

#[path = "transitions_sim_switch.rs"]
mod switch;
pub use switch::{Switch, SwitchLog};

/// The proof rule, the simulated render verdicts and `recheck_registration` (R8-detect-2,
/// -5, -6), split out for file size.
#[path = "transitions_sim_proof.rs"]
mod proof;
