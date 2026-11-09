//! The unified record mode (no args, `--record`, `--record-only`): Recall's
//! always-on controller. One Win32 pump thread owns the tray, hotkeys,
//! foreground watcher, capture-target tracker, the optional HDR manager, the
//! overlay process and the engine supervisor; the control pipe feeds it
//! commands (`record_cmds`).
//!
//! - Records ANY game: the foreground fullscreen window (foreground hook, a
//!   100 ms fullscreen check of the foreground, 1 s re-probe; `record_detect`)
//!   becomes the target after 300 ms (2 s to replace another game); the
//!   engine is (re)started with that window, name and game-audio pid; no
//!   target -> engine stopped ("waiting").
//! - Never touches a display setting unless `behavior.hdr_auto` (see
//!   `record_hdr`); starts fine without the managed monitor.
//! - Env vars still override config at startup (`Start-Recall.ps1`), and
//!   `RECALL_CAPTURE_HWND` pins one window while its process lives.
//! - Pause (tray "Stop recording" / pipe `pause`): the engine stops at once
//!   (no linger) -- after a running long recording is stopped and saved (up
//!   to 10 s) -- games are ignored and capture actions are refused until
//!   resume; runtime only. Hotkeys: `record_actions`,
//!   `record_hotkeys`. Saves / long recording in flight defer
//!   settings restarts and retargets (`record_activity`); the storage cap
//!   runs in `record_storage`.

use std::sync::mpsc::{channel, Receiver};
use std::sync::Arc;
use std::time::{Duration, Instant};

use windows::Win32::UI::WindowsAndMessaging::{MsgWaitForMultipleObjectsEx, MWMO_INPUTAVAILABLE, QS_ALLINPUT};

use crate::app;
use crate::config::Config;
use crate::control::{Inbound, PipeHub};
use crate::hotkey::RecordHotkeys;
use crate::liveness::ProcessWatch;
use crate::log;
use crate::overlay_proc::OverlayProc;
use crate::record_activity::{EngineActivity, PendingPause};
use crate::record_config::ConfigStore;
use crate::record_detect::Follow;
pub(crate) use crate::record_env::apply_affinity;
use crate::record_env::EnvOverrides;
use crate::record_hdr::HdrManager;
use crate::record_engine::{self, EnginePlanner, EngineTarget, EXIT_LINGER};
use crate::record_notice::NoticeTracker;
use crate::record_quit::QuitReason;
use crate::record_render::RenderCheck;
use crate::record_janitor::Janitor;
use crate::record_startup::{apply_niceness, pinned_target_from_env};
use crate::record_target::{Target, TargetTracker, START_DEBOUNCE, TARGET_DEBOUNCE};
use crate::record_tray::{RecallUi, RecordingTrayCmd};
use crate::record_tray_slot::TraySlot;
use crate::supervisor::Supervisor;
use crate::watcher::{WatchEvent, Watcher};

/// Pump wake cap: bounds pipe-command latency (commands arrive on a channel,
/// not as window messages).
const MAX_WAIT_MS: u32 = 50;
/// Foreground re-probe cadence (catches games that go fullscreen after focus).
const PROBE_EVERY: Duration = Duration::from_secs(1);
/// Read-only HDR state refresh while HDR management is off.
const HDR_REFRESH_EVERY: Duration = Duration::from_secs(5);

/// The controller's state, owned by the pump thread. `record_cmds` adds the
/// pipe/tray/hotkey command handlers.
pub struct Controller {
    pub(crate) store: ConfigStore,
    /// What `config.toml` says (what `set_config` patches and saves).
    pub(crate) file_cfg: Config,
    /// config.toml as last read or written (a hand edit since then is merged, `config_watch`).
    pub(crate) config_stamp: Option<crate::config_watch::FileStamp>,
    /// The last load's repairs of config.toml, sent with the `config` event (`config_repairs`).
    pub(crate) config_repairs: crate::config_repairs::ConfigRepairs,
    pub(crate) overrides: EnvOverrides,
    /// `file_cfg` + startup env overrides: what is running.
    pub(crate) cfg: Config,
    pub(crate) supervisor: Supervisor,
    pub(crate) hub: Arc<PipeHub>,
    /// Retried until Explorer's notification area takes it (`record_tray_slot`).
    pub(crate) tray: TraySlot,
    pub(crate) hotkeys: Option<RecordHotkeys>,
    pub(crate) overlay: OverlayProc,
    pub(crate) ui: RecallUi,
    pub(crate) hdr: HdrManager,
    pub(crate) tracker: TargetTracker,
    pub(crate) planner: EnginePlanner,
    pub(crate) pinned: Option<Target>,
    target_watch: ProcessWatch,
    pin_watch: ProcessWatch,
    /// The microphone state we enforce on every engine instance.
    pub(crate) mic_muted: bool,
    /// The capture tonemap selector we re-apply on every engine instance.
    pub(crate) mode_select: Option<String>,
    /// The CPU mask this process actually runs with (passed on to the engine).
    pub(crate) applied_mask: Option<String>,
    seen_hello: u32,
    pub(crate) hello_at_retarget: u32,
    pub(crate) last_session: String,
    pub(crate) last_hdr_event: String,
    /// Last time each hotkey action ran (per-action debounce).
    pub(crate) last_fired: std::collections::HashMap<&'static str, Instant>,
    /// Hotkeys that did not register (taken by another app, invalid, duplicate): status `hotkey_problems`.
    pub(crate) hotkey_problems: Vec<String>,
    /// A shortcut notice for an overlay still starting: sent with the next
    /// late-joiner status, dropped after its deadline.
    pub(crate) pending_notice: Option<(String, Instant)>,
    pub(crate) last_probe: Instant,
    last_hdr_refresh: Instant,
    /// The last foreground fed to the tracker (`record_detect::fed_key`).
    pub(crate) last_fed: Option<crate::record_detect::FedKey>,
    /// The foreground window if it could become a target by going fullscreen.
    pub(crate) followable: Option<isize>,
    /// The followed foreground's fullscreen verdict (`record_detect`).
    pub(crate) follow: Follow,
    /// When the session window was first seen hidden (`record_visibility`).
    pub(crate) hidden: crate::record_visibility::HiddenWatch,
    /// (hwnd, wait) of the last committed target, for the retarget log line.
    pub(crate) detected: Option<(isize, Duration)>,
    pub(crate) target_monitor: isize,
    /// The primary monitor, re-read every 5 s (`record_monitor`).
    pub(crate) primary: crate::record_monitor::PrimaryWatch,
    /// Set once a quit is asked for; the reason decides whether the app's windows close.
    pub(crate) quit: Option<QuitReason>,
    /// Runtime pause (tray "Stop recording" / pipe `pause`): no engine, game
    /// targets ignored, capture actions refused. A restart resumes.
    pub(crate) paused: bool,
    pub(crate) setup_owner: Option<u32>,
    pub(crate) owner_watch: Option<crate::record_setup_owner::OsOwner>,
    /// Saves / long recording in flight (from engine events).
    pub(crate) activity: EngineActivity,
    pub(crate) engine_events: Receiver<serde_json::Value>,
    /// A settings / resume restart waits for a save, long recording or drain: restart once idle.
    pub(crate) pending_restart: Option<crate::record_restart::RestartReason>,
    /// A retarget fell due while busy (the planner holds it until idle).
    pub(crate) retarget_held: bool,
    /// The closed game's drain as status (`record_drain`).
    pub(crate) drain_clock: crate::record_drain::DrainClock,
    /// Pause asked during a long recording: waiting for it to stop.
    pub(crate) pending_pause: Option<PendingPause>,
    pub(crate) janitor: Janitor,
    /// Recall Island notices for recording changes (`record_notice`).
    pub(crate) notices: NoticeTracker,
    /// A game session stuck in `starting` (`record_capture_watch`).
    pub(crate) capture_stuck: crate::record_capture_watch::StuckWatch,
    /// Exclusive fullscreen hiding the island (`record_fullscreen`).
    fullscreen: crate::record_fullscreen::FullscreenWatch,
    /// The island editor shortcut waiting for the overlay's answer.
    pub(crate) editor_ask: crate::record_editor_ask::EditorAsk,
    /// This token may start the overlay's frame-timing ETW session
    /// (`frame_timing`; checked once, membership changes need a new sign-in).
    pub(crate) can_trace: bool,
    /// Unknown fullscreen candidates must render like a game (`record_render`).
    pub(crate) render: RenderCheck,
    /// Sleep and resume (`record_power`).
    pub(crate) power: crate::record_power::PowerPlanner,
    /// Engine restart after a lasting encoder failure (`record_recover`).
    pub(crate) recovery: crate::record_recover::EncoderRecovery,
    /// A recovery restart waiting for the long recording to end early (`record_recover`).
    pub(crate) ending_early: Option<crate::record_recover::EndingEarly>,
    /// Win+L during a long recording and a session end in progress (`record_session_host`).
    pub(crate) session_watch: crate::record_session_host::SessionState,
    /// The clean shutdown already ran (a session end runs it from `WM_ENDSESSION`).
    shut_down: bool,
    /// A quit waiting for the save being written (`record_quit_wait`).
    pub(crate) quit_wait: crate::record_quit_wait::QuitWait,
    /// The shutdown block reason, held while a save is written (`record_session_end`).
    pub(crate) block_reason: crate::record_session_save::BlockReason,
    pub(crate) session_end: crate::record_session_save::SessionEnd,
}

/// Entry point. Returns the process exit code.
pub fn run() -> i32 {
    let store = ConfigStore::user();
    let file_cfg = store.load();
    let overrides = EnvOverrides::from_env();
    let mut cfg = overrides.apply(&file_cfg);
    // Never record into a linked folder: logged, and shown to every client that joins in the
    // first minute (the overlay starts after this).
    let folder_notice = crate::record_link_guard::guard(&mut cfg, None, &crate::record_link_guard::read_links());
    if !overrides.is_empty() {
        log!("record: startup environment overrides active: {overrides:?}");
    }
    // Pin ourselves BEFORE any child exists: engine, overlay and UI inherit it.
    let applied_mask = apply_niceness(&cfg.recording.cpu_mask);
    let pinned = pinned_target_from_env();

    // A tray that fails (Explorer not ready at sign-in) never stops the recorder: the pump retries it.
    let tray = TraySlot::recording(Instant::now());
    let target_monitor = app::primary_monitor_isize();
    let (watch_tx, watch_rx) = channel::<WatchEvent>();
    let watcher = Watcher::install(watch_tx, target_monitor)
        .map_err(|e| log!("record: watcher install failed ({e}); relying on the 1 s foreground probe"))
        .ok();
    let (ctl_tx, ctl_rx) = channel::<Inbound>();
    let hub = PipeHub::start(ctl_tx);
    let supervisor = Supervisor::start_idle();
    supervisor.attach_hub(hub.clone());
    let (engine_tx, engine_events) = channel::<serde_json::Value>();
    supervisor.attach_events(engine_tx);
    let janitor = Janitor::start(hub.clone());
    let mut hdr = HdrManager::probe(&cfg);
    if !crate::hdr_gate::enabled() {
        log!("record: HDR features shelved (experimental); no display setting will change");
    } else if cfg.behavior.hdr_auto {
        if let Err(e) = hdr.activate(&cfg) {
            log!("record: hdr_auto is on but HDR management could not start: {e}");
        }
    } else {
        log!("record: HDR management off (behavior.hdr_auto = false); no display setting will change");
    }

    let now = Instant::now();
    let can_trace = crate::frame_timing::can_trace();
    let setup_owner = std::env::var("RECALL_SETUP_OWNER_PID").ok()
        .and_then(|pid| pid.parse::<u32>().ok()).filter(|&pid| pid != 0);
    let mut ctl = Controller {
        config_stamp: crate::config_watch::FileStamp::of(store.path()),
        config_repairs: crate::config_repairs::ConfigRepairs::after_load(store.path()),
        store,
        file_cfg,
        overrides,
        cfg,
        supervisor,
        hub,
        tray,
        hotkeys: None,
        overlay: OverlayProc::new(),
        ui: RecallUi::from_current_exe(),
        hdr,
        tracker: TargetTracker::new(TARGET_DEBOUNCE, START_DEBOUNCE),
        planner: EnginePlanner::new(EXIT_LINGER),
        pinned,
        target_watch: ProcessWatch::new("record: target"),
        pin_watch: ProcessWatch::new("record: pinned target"),
        mic_muted: false,
        mode_select: None,
        applied_mask,
        seen_hello: 0,
        hello_at_retarget: 0,
        last_session: String::new(),
        last_hdr_event: String::new(),
        last_fired: Default::default(),
        hotkey_problems: Vec::new(),
        pending_notice: folder_notice.map(|n| (crate::record_notice::shortcut_notice_json("Recordings folder", &n, true, true), now + Duration::from_secs(60))),
        last_probe: now,
        last_hdr_refresh: now,
        last_fed: None,
        followable: None,
        follow: Follow::new(now),
        hidden: Default::default(),
        detected: None,
        target_monitor,
        primary: crate::record_monitor::PrimaryWatch::new(crate::record_monitor::read_primary(), now),
        quit: None,
        paused: setup_owner.is_some(),
        owner_watch: setup_owner.map(crate::record_setup_owner::OsOwner::open),
        setup_owner,
        activity: EngineActivity::default(),
        engine_events,
        pending_restart: None,
        retarget_held: false,
        drain_clock: Default::default(),
        pending_pause: None,
        janitor,
        notices: NoticeTracker::new(),
        capture_stuck: Default::default(),
        fullscreen: Default::default(),
        editor_ask: Default::default(),
        can_trace,
        render: RenderCheck::new(can_trace),
        power: Default::default(),
        recovery: Default::default(),
        ending_early: None,
        session_watch: Default::default(),
        shut_down: false,
        quit_wait: Default::default(),
        block_reason: Default::default(),
        session_end: Default::default(),
    };
    if !ctl.can_trace {
        log!("record: FPS needs Performance Log Users (run the installer, then sign in again); stats show FPS as -");
    }
    ctl.rebind_hotkeys();
    ctl.overlay.sync(&ctl.cfg.overlay);
    ctl.probe_foreground(now, true);
    crate::record_power_win::register();
    crate::record_session_win::start();
    log!(
        "record: controller up (auto_record={}, buffer={}s, save={}, mic={}, out={:?}, storage cap={} GiB)",
        ctl.cfg.recording.auto_record,
        ctl.cfg.recording.buffer_secs,
        ctl.cfg.recording.save_hotkey,
        ctl.cfg.recording.mic_hotkey,
        ctl.cfg.recording.output_dir,
        ctl.cfg.storage.cap_gb
    );

    let code = ctl.pump(&watch_rx, &ctl_rx);
    if let Some(w) = watcher {
        w.uninstall(); // on the install (pump) thread
    }
    ctl.shutdown();
    code
}

impl Controller {
    fn pump(&mut self, watch_rx: &Receiver<WatchEvent>, ctl_rx: &Receiver<Inbound>) -> i32 {
        loop {
            let timeout = crate::record_quit_wait::pump_wait_ms(self.quit.is_some(), self.next_deadline(), Instant::now(), MAX_WAIT_MS);
            unsafe {
                MsgWaitForMultipleObjectsEx(None, timeout, QS_ALLINPUT, MWMO_INPUTAVAILABLE);
            }
            // Drives the WinEvent hook callback, tray menu, hotkeys and the session window.
            if crate::record_session_win::drain_messages(self) {
                if self.quit.is_none() {
                    log!("record: WM_QUIT received");
                }
                self.quit.get_or_insert(QuitReason::WindowsQuit);
            }
            if self.shut_down {
                return 0; // the session ended (`WM_ENDSESSION`)
            }
            let now = Instant::now();
            while let Ok(ev) = watch_rx.try_recv() {
                self.on_foreground_event(&ev, now);
            }
            if now.duration_since(self.last_probe) >= PROBE_EVERY {
                self.probe_foreground(now, false);
            }
            for cmd in self.tray.poll() {
                self.on_tray(cmd);
            }
            if let Some(fired) = self.hotkeys.as_ref().map(|h| h.poll()) {
                self.on_hotkeys(fired);
            }
            self.hotkey_retry_tick(now);
            while let Ok(inbound) = ctl_rx.try_recv() {
                self.handle_inbound(inbound);
            }
            self.session_end_tick(now);
            // A quit waits for the save being written (`record_quit_wait`), then returns.
            if self.quit.is_some() {
                if self.quit_ready(now) {
                    return 0;
                }
                self.quitting_tick(now);
                continue;
            }
            self.power_tick(now);
            self.session_tick(now);
            self.tick(now);
        }
    }

    fn next_deadline(&self) -> Option<Instant> {
        [
            self.tracker.next_deadline(),
            self.planner.linger_deadline(),
            self.hdr.next_deadline(),
            self.janitor.next_deadline(),
            self.overlay.deadline(),
        ]
        .into_iter()
        .flatten()
        .min()
    }

    pub(crate) fn wanted(&self) -> EngineTarget {
        record_engine::wanted(self.tracker.current(), self.pinned.as_ref(), &self.cfg.recording, self.paused)
    }

    /// Engine events -> liveness (process + window) -> followed
    /// foreground -> target -> engine -> HDR ->
    /// fresh-engine intents -> deferred restart / storage -> overlay -> status
    /// (+ island notices).
    fn tick(&mut self, now: Instant) {
        self.drain_engine_events(now);
        self.monitor_tick(now);
        if let Some(pid) = self.pin_watch.poll(self.pinned.as_ref().map(|t| t.pid)) {
            log!("record: pinned target's process {pid} exited; auto-detection resumes");
            self.pinned = None;
            self.tracker.on_process_exit(pid);
        }
        if let Some(pid) = self.target_watch.poll(self.tracker.current().map(|t| t.pid)) {
            self.tracker.on_process_exit(pid);
        }
        self.check_windows();
        self.follow_tick(now);
        self.render_tick();
        if self.poll_tracker(now) {
            self.note_commit();
        }
        self.plan_engine(now);
        self.hdr.tick(now);
        let hellos = self.supervisor.hello_count();
        if hellos != self.seen_hello {
            self.seen_hello = hellos;
            // The old engine's exit is queued before the new hello: read it first, so the saves
            // it lost are reported before the slate is wiped.
            self.drain_engine_events(now);
            self.activity.engine_restarted();
            self.reapply_engine_intents();
        }
        self.setup_owner_tick();
        self.activity_tick(now);
        self.encoder_recover_tick(now);
        self.overlay.sync(&self.cfg.overlay);
        self.fullscreen_tick(now);
        self.island_editor_tick(now);
        self.capture_stuck_tick(now);
        if !self.hdr.is_active() && now.duration_since(self.last_hdr_refresh) >= HDR_REFRESH_EVERY {
            self.last_hdr_refresh = now;
            self.hdr.refresh_live();
        }
        self.publish_status();
    }

    /// While the island runs: a recorded game in exclusive fullscreen hides it;
    /// say so once per game, as soon as the island can be seen again.
    fn fullscreen_tick(&mut self, now: Instant) {
        use crate::record_fullscreen::{exclusive_now, notice_json, Hidden};
        if !self.cfg.overlay.wants_process() || !self.fullscreen.due(now) {
            return;
        }
        let game = match self.wanted() {
            EngineTarget::Window(t) if crate::watcher::is_foreground(t.hwnd) => Some(Hidden { exe: t.exe, name: t.name }),
            _ => None,
        };
        let exclusive = game.is_some() && exclusive_now();
        if let Some(hidden) = self.fullscreen.observe(game, exclusive) {
            log!("record: {} ran in exclusive fullscreen (the island cannot show there); told the user", hidden.exe);
            self.hub.event("session", &notice_json(&hidden));
        }
    }


    /// The hotkey manager, created on first use (pump thread).
    pub(crate) fn ensure_hotkeys(&mut self) -> Option<&mut RecordHotkeys> {
        if self.hotkeys.is_none() {
            self.hotkeys = RecordHotkeys::new().map_err(|e| log!("record: hotkeys unavailable: {e}")).ok();
        }
        self.hotkeys.as_mut()
    }

    fn on_tray(&mut self, cmd: RecordingTrayCmd) {
        match cmd {
            RecordingTrayCmd::Open => {
                let matched = self.ui.open();
                log!("record: tray open, matched UI windows={}", matched.len());
            }
            RecordingTrayCmd::TogglePause => self.set_paused(!self.paused),
            RecordingTrayCmd::AddGame => self.add_offered_game(),
            RecordingTrayCmd::Quit => {
                log!("record: tray quit");
                self.ask_quit(QuitReason::TrayMenu, false);
            }
        }
    }

    /// Ordered shutdown (after `quit_ready` let a save in flight finish): display
    /// restore first (if HDR was managed), then the UI, then the engine, then the
    /// overlay (and its frame-timing session): the island shows the save to its end.
    /// Only the tray Quit closes the app; any other quit leaves it open to reconnect
    /// to the next recorder.
    fn shutdown(mut self) {
        self.shutdown_now(crate::supervisor::QUIT_GRACE);
    }

    /// The shutdown in place, once; `grace` caps the engine's quit. A session end runs it
    /// from `WM_ENDSESSION` (`record_session_win`).
    pub(crate) fn shutdown_now(&mut self, grace: Duration) {
        if std::mem::replace(&mut self.shut_down, true) {
            return;
        }
        crate::record_session_win::stop();
        crate::record_power_win::unregister();
        self.render.stop();
        self.hdr.shutdown();
        let reason = self.quit.unwrap_or(QuitReason::WindowsQuit);
        if reason.closes_ui() {
            let matched = self.ui.close();
            log!("record: requested WM_CLOSE for {} Recall UI window(s)", matched.len());
        } else {
            log!("record: {} leaves the Recall app open", reason.label());
        }
        self.hotkeys = None;
        self.supervisor.stop(grace);
        self.overlay.stop();
        log!("record: clean shutdown (pipe lines dropped for stalled clients: {})", self.hub.dropped());
    }
}
