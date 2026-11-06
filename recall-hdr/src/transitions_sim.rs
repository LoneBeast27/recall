//! Test harness for the transition matrix (`transitions_tests`): the pure pieces the record
//! controller (`record_app::Controller`) runs every pump tick, composed in the same order,
//! with injected time and window state. No Win32, no engine process, no audio.
//!
//! Which live call site each step mirrors:
//! - [`Sim::focus`]: `record_detect::on_foreground_event` (classify -> `TargetTracker::on_foreground`).
//!   The render check is not run: the games here commit like known games / overrides do
//!   (`TargetTracker::poll`, i.e. `poll_admit` answering `Admit::Commit`).
//! - [`Sim::exit`] / [`Sim::window_gone`]: `record_app::tick` (`target_watch.poll` ->
//!   `on_process_exit`) and `record_detect::check_windows` (`on_window_gone`).
//! - [`Sim::tick`]: `record_app::tick`: `poll_tracker` -> `record_actions::plan_engine`
//!   (`set_drain_len(drain_len(..))`, `wanted()`, `activity.retarget_busy`, `planner.step`) ->
//!   fresh-engine bookkeeping (`activity.engine_restarted` on the new engine's hello) ->
//!   `activity_tick` (pending pause) -> `record_cmds::announce` (island notices).
//! - [`Sim::save`]: `record_cmds::save_clip`; [`Sim::long_start`]: `engine_command("record_start")`;
//!   [`Sim::pause`] / [`Sim::resume`]: `record_actions::set_paused` / `apply_pause`.
//!
//! Every `Step::Apply` is one engine (re)spawn or stop (`Supervisor::retarget`), and every
//! spawn starts a new replay ring: "buffer kept" = no `Apply` since the mark.

use std::time::{Duration, Instant};

use crate::config::Config;
use crate::record_activity::{EngineActivity, LongRec, PendingPause};
use crate::record_config::RecordingCfg;
use crate::record_drain::drain_len;
use crate::record_engine::{capture_refusal, wanted, EnginePlanner, EngineTarget, Step, EXIT_LINGER};
use crate::record_notice::{NoticeTracker, Situation};
use crate::record_target::{classify_event, Target, TargetTracker, START_DEBOUNCE, TARGET_DEBOUNCE};
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
        let candidate = classify_event(&ev, &self.cfg);
        self.foreground = Some(ev);
        self.tracker.on_foreground(candidate, self.now);
    }

    /// A foreground the watcher cannot identify (OpenProcess fails): `watcher::build_event`
    /// returns `None`, so no event reaches the tracker at all (the hook and the 1 s probe
    /// both drop it). Kept as a step so the tests read like the timeline.
    pub fn focus_unidentified(&mut self) {
        self.foreground = None;
    }

    pub fn exit(&mut self, pid: u32) {
        self.tracker.on_process_exit(pid);
    }

    pub fn window_gone(&mut self, hwnd: isize) {
        self.tracker.on_window_gone(hwnd);
    }

    pub fn wanted(&self) -> EngineTarget {
        wanted(self.tracker.current(), None, &self.rec, self.paused)
    }

    pub fn engine(&self) -> &EngineTarget {
        self.planner.applied()
    }

    pub fn tick(&mut self) {
        let now = self.now;
        if self.pending_pause.is_some_and(|p| p.ready(&self.activity, now)) {
            self.apply_pause(true);
        }
        self.tracker.poll(now);
        self.planner.set_drain_len(drain_len(self.buffered, self.rec.buffer_secs));
        let busy = self.activity.retarget_busy(now);
        match self.planner.step(&self.wanted(), now, busy) {
            Step::Apply(target) => self.applied_engine(target),
            Step::Held => self.held_ticks += 1,
            // `plan_engine`: one record_stop per recording, the retarget follows its stop.
            Step::StopLongRec { game_closed } => {
                if self.activity.auto_stop(now, game_closed) {
                    self.sent.push(("record_stop".into(), self.engine().clone()));
                }
            }
            Step::Keep => {}
        }
        self.announce();
    }

    fn applied_engine(&mut self, target: EngineTarget) {
        if target != EngineTarget::Off {
            // The new engine's hello (`record_app::tick`): nothing of the old one is in flight.
            self.activity.engine_restarted();
        }
        self.applied.push((self.now, target));
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
            self.applied.push((self.now, EngineTarget::Off));
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
