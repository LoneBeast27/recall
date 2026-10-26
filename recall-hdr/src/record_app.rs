//! The unified record mode (no args, `--record`, `--record-only`): Recall's
//! always-on controller. One Win32 pump thread owns the tray, hotkeys,
//! foreground watcher, capture-target tracker, the optional HDR manager, the
//! overlay process and the engine supervisor; the control pipe feeds it
//! commands (`record_cmds`).
//!
//! - Records ANY game: the foreground fullscreen window (hook + 1 s re-probe)
//!   becomes the target after 2 s; the engine is (re)started with that window,
//!   name and game-audio pid; no target -> engine stopped ("waiting").
//! - Never touches a display setting unless `behavior.hdr_auto` (see
//!   `record_hdr`); starts fine without the managed monitor.
//! - Env vars still override config at startup (`Start-Recall.ps1`), and
//!   `RECALL_CAPTURE_HWND` pins one window while its process lives.
//! - Pause (tray "Stop recording" / pipe `pause`): the engine stops at once
//!   (no linger) -- after a running long recording is stopped and saved (up
//!   to 10 s) -- games are ignored and capture actions are refused until
//!   resume; runtime only. Hotkeys + the advanced chord: `record_actions`,
//!   `record_hotkeys`, `record_chord`. Saves / long recording in flight defer
//!   settings restarts and retargets (`record_activity`); the storage cap
//!   runs in `record_storage`.

use std::sync::mpsc::{channel, Receiver};
use std::sync::Arc;
use std::time::{Duration, Instant};

use windows::Win32::System::Threading::{
    GetCurrentProcess, SetPriorityClass, SetProcessAffinityMask, BELOW_NORMAL_PRIORITY_CLASS,
};
use windows::Win32::UI::WindowsAndMessaging::{MsgWaitForMultipleObjectsEx, MWMO_INPUTAVAILABLE, QS_ALLINPUT};

use crate::app;
use crate::config::Config;
use crate::control::{Inbound, PipeHub};
use crate::engine_policy::EngineSpec;
use crate::hotkey::RecordHotkeys;
use crate::liveness::ProcessWatch;
use crate::log;
use crate::overlay_proc::OverlayProc;
use crate::record_activity::{EngineActivity, PendingPause};
use crate::record_chord::Chord;
use crate::record_config::{parse_cpu_mask, ConfigStore};
use crate::record_env::EnvOverrides;
use crate::record_hdr::HdrManager;
use crate::record_engine::{self, engine_env, EnginePlanner, EngineTarget, EXIT_LINGER};
use crate::record_notice::NoticeTracker;
use crate::record_storage::Janitor;
use crate::record_target::{classify_event, display_name, sanitize, Target, TargetTracker, TARGET_DEBOUNCE};
use crate::record_tray::{RecallUi, RecordingTray, RecordingTrayCmd};
use crate::supervisor::{EngineHealth, Supervisor};
use crate::watcher::{self, WatchEvent, Watcher};

/// Pump wake cap: bounds pipe-command latency (commands arrive on a channel,
/// not as window messages).
const MAX_WAIT_MS: u32 = 50;
/// Foreground re-probe cadence (catches games that go fullscreen after focus).
const PROBE_EVERY: Duration = Duration::from_secs(1);
/// Read-only HDR state refresh while HDR management is off.
const HDR_REFRESH_EVERY: Duration = Duration::from_secs(5);
const FALLBACK_CPU_MASK: usize = 0xffff_0000;

/// The controller's state, owned by the pump thread. `record_cmds` adds the
/// pipe/tray/hotkey command handlers.
pub struct Controller {
    pub(crate) store: ConfigStore,
    /// What `config.toml` says (what `set_config` patches and saves).
    pub(crate) file_cfg: Config,
    pub(crate) overrides: EnvOverrides,
    /// `file_cfg` + startup env overrides: what is running.
    pub(crate) cfg: Config,
    pub(crate) supervisor: Supervisor,
    pub(crate) hub: Arc<PipeHub>,
    pub(crate) tray: RecordingTray,
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
    last_probe: Instant,
    last_hdr_refresh: Instant,
    /// (hwnd, is_target) of the last foreground fed to the tracker.
    pub(crate) last_fed: Option<(isize, bool)>,
    target_monitor: isize,
    pub(crate) quit: bool,
    /// Runtime pause (tray "Stop recording" / pipe `pause`): no engine, game
    /// targets ignored, capture actions refused. A restart resumes.
    pub(crate) paused: bool,
    /// Saves / long recording in flight (from engine events).
    pub(crate) activity: EngineActivity,
    pub(crate) engine_events: Receiver<serde_json::Value>,
    /// Engine settings changed while busy: restart once idle.
    pub(crate) pending_restart: bool,
    /// A retarget fell due while busy (the planner holds it until idle).
    pub(crate) retarget_held: bool,
    /// Pause asked during a long recording: waiting for it to stop.
    pub(crate) pending_pause: Option<PendingPause>,
    pub(crate) janitor: Janitor,
    /// The advanced-shortcut chord window.
    pub(crate) chord: Chord,
    /// How the leader reads on this keyboard layout ("Alt `").
    pub(crate) leader_label: String,
    /// Recall Island notices for recording changes (`record_notice`).
    pub(crate) notices: NoticeTracker,
    /// This token may start the overlay's frame-timing ETW session
    /// (`frame_timing`; checked once, membership changes need a new sign-in).
    pub(crate) can_trace: bool,
}

/// Entry point. Returns the process exit code.
pub fn run() -> i32 {
    let store = ConfigStore::user();
    let file_cfg = store.load();
    let overrides = EnvOverrides::from_env();
    let cfg = overrides.apply(&file_cfg);
    if !overrides.is_empty() {
        log!("record: startup environment overrides active: {overrides:?}");
    }
    // Pin ourselves BEFORE any child exists: engine, overlay and UI inherit it.
    let applied_mask = apply_niceness(&cfg.recording.cpu_mask);
    let pinned = pinned_target_from_env();

    let tray = match RecordingTray::build() {
        Ok(t) => t,
        Err(e) => {
            log!("record: FATAL -- tray unavailable: {e}");
            return 2;
        }
    };
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
    if cfg.behavior.hdr_auto {
        if let Err(e) = hdr.activate(&cfg) {
            log!("record: hdr_auto is on but HDR management could not start: {e}");
        }
    } else {
        log!("record: HDR management off (behavior.hdr_auto = false); no display setting will change");
    }

    let now = Instant::now();
    let mut ctl = Controller {
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
        tracker: TargetTracker::new(TARGET_DEBOUNCE),
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
        last_probe: now,
        last_hdr_refresh: now,
        last_fed: None,
        target_monitor,
        quit: false,
        paused: false,
        activity: EngineActivity::default(),
        engine_events,
        pending_restart: false,
        retarget_held: false,
        pending_pause: None,
        janitor,
        chord: Chord::new(Duration::from_millis(1_500), global_hotkey::hotkey::Modifiers::ALT),
        leader_label: String::new(),
        notices: NoticeTracker::new(),
        can_trace: crate::frame_timing::can_trace(),
    };
    if !ctl.can_trace {
        log!("record: FPS needs Performance Log Users (run the installer, then sign in again); stats show FPS as -");
    }
    ctl.rebind_hotkeys();
    ctl.overlay.sync(&ctl.cfg.overlay);
    ctl.probe_foreground(now, true);
    log!(
        "record: controller up (auto_record={}, buffer={}s, save={}, mic={}, advanced={} via {}, out={:?}, storage cap={} GiB)",
        ctl.cfg.recording.auto_record,
        ctl.cfg.recording.buffer_secs,
        ctl.cfg.recording.save_hotkey,
        ctl.cfg.recording.mic_hotkey,
        ctl.cfg.hotkeys.advanced_enabled,
        ctl.leader_label,
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
            let timeout = app::wait_ms(self.next_deadline(), MAX_WAIT_MS);
            unsafe {
                MsgWaitForMultipleObjectsEx(None, timeout, QS_ALLINPUT, MWMO_INPUTAVAILABLE);
            }
            // Drives the WinEvent hook callback, tray menu, and hotkeys.
            if app::drain_messages() {
                log!("record: WM_QUIT received");
                return 0;
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
            while let Ok(inbound) = ctl_rx.try_recv() {
                self.handle_inbound(inbound);
            }
            if self.quit {
                return 0;
            }
            self.tick(now);
        }
    }

    fn next_deadline(&self) -> Option<Instant> {
        [
            self.tracker.next_deadline(),
            self.planner.linger_deadline(),
            self.hdr.next_deadline(),
            self.chord.deadline(),
            self.janitor.next_deadline(),
        ]
        .into_iter()
        .flatten()
        .min()
    }

    fn on_foreground_event(&mut self, ev: &WatchEvent, now: Instant) {
        let candidate = classify_event(ev, &self.cfg);
        self.last_fed = Some((ev.hwnd, candidate.is_some()));
        self.tracker.on_foreground(candidate, now);
        self.hdr.on_foreground(ev, &self.cfg, now);
    }

    /// Re-read the foreground; feed it only when (window, verdict) changed.
    /// Also drops a target / pin whose window no longer exists.
    pub(crate) fn probe_foreground(&mut self, now: Instant, initial: bool) {
        self.last_probe = now;
        if let Some(t) = self.pinned.as_ref().filter(|t| !watcher::window_alive(t.hwnd)) {
            log!("record: pinned window {:#x} is gone; auto-detection resumes", t.hwnd);
            self.pinned = None;
        }
        if let Some(hwnd) = self.tracker.current().map(|t| t.hwnd).filter(|h| !watcher::window_alive(*h)) {
            log!("record: target window {hwnd:#x} is gone");
            self.tracker.on_window_gone(hwnd);
        }
        let Some(ev) = watcher::probe_foreground(self.target_monitor) else { return };
        if initial {
            self.on_foreground_event(&ev, now);
            return;
        }
        let candidate = classify_event(&ev, &self.cfg);
        let key = (ev.hwnd, candidate.is_some());
        if self.last_fed != Some(key) {
            self.last_fed = Some(key);
            self.tracker.on_foreground(candidate, now);
        }
    }

    pub(crate) fn wanted(&self) -> EngineTarget {
        record_engine::wanted(self.tracker.current(), self.pinned.as_ref(), &self.cfg.recording, self.paused)
    }

    /// Engine events -> chord -> liveness -> target -> engine -> HDR ->
    /// fresh-engine intents -> deferred restart / storage -> overlay -> status
    /// (+ island notices).
    fn tick(&mut self, now: Instant) {
        self.drain_engine_events(now);
        self.chord_tick(now);
        if let Some(pid) = self.pin_watch.poll(self.pinned.as_ref().map(|t| t.pid)) {
            log!("record: pinned target's process {pid} exited; auto-detection resumes");
            self.pinned = None;
            self.tracker.on_process_exit(pid);
        }
        if let Some(pid) = self.target_watch.poll(self.tracker.current().map(|t| t.pid)) {
            self.tracker.on_process_exit(pid);
        }
        self.tracker.poll(now);
        self.plan_engine(now);
        self.hdr.tick(now);
        let hellos = self.supervisor.hello_count();
        if hellos != self.seen_hello {
            self.seen_hello = hellos;
            self.activity.engine_restarted();
            self.reapply_engine_intents();
        }
        self.activity_tick(now);
        self.overlay.sync(&self.cfg.overlay);
        if !self.hdr.is_active() && now.duration_since(self.last_hdr_refresh) >= HDR_REFRESH_EVERY {
            self.last_hdr_refresh = now;
            self.hdr.refresh_live();
        }
        self.publish_status();
    }

    /// Start, restart or stop the engine for `target` (an intended restart).
    pub(crate) fn apply_engine_target(&mut self, target: &EngineTarget) {
        self.hello_at_retarget = self.supervisor.hello_count();
        // Any (re)start or stop picks up the current settings.
        self.pending_restart = false;
        match target {
            EngineTarget::Off => {
                log!("record: no target -> engine stopped (waiting for a game)");
                self.supervisor.retarget(None);
            }
            EngineTarget::Window(t) => {
                log!("record: target -> {:?} ({}, pid {}, hwnd {:#x})", t.name, t.exe, t.pid, t.hwnd);
                self.supervisor.retarget(Some(self.engine_spec(target)));
            }
            EngineTarget::Desktop => {
                log!("record: no game -> desktop capture");
                self.supervisor.retarget(Some(self.engine_spec(target)));
            }
        }
    }

    fn engine_spec(&self, target: &EngineTarget) -> EngineSpec {
        EngineSpec { env: engine_env(target, &self.cfg.recording, self.applied_mask.as_deref()) }
    }

    /// Restart a running engine so new buffer/output/affinity settings apply.
    pub(crate) fn restart_engine_for_settings(&mut self) {
        if *self.planner.applied() == EngineTarget::Off {
            return; // the next start already uses the new settings
        }
        let wanted = self.wanted();
        self.planner.force(wanted.clone());
        self.apply_engine_target(&wanted);
    }

    /// A fresh engine instance starts unmuted in `auto` mode: re-apply ours.
    fn reapply_engine_intents(&mut self) {
        if self.mic_muted {
            self.supervisor.send_engine_line(r#"{"cmd":"set_mic_mute","muted":true}"#);
        }
        if let Some(mode) = self.mode_select.as_deref().filter(|m| *m != "auto") {
            self.supervisor.send_engine_line(&serde_json::json!({"cmd": "set_mode", "value": mode}).to_string());
        }
    }

    pub(crate) fn engine_running(&self) -> bool {
        matches!(self.supervisor.health(), EngineHealth::Running { pid } if pid != 0)
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
            RecordingTrayCmd::Quit => {
                log!("record: tray quit");
                self.quit = true;
            }
        }
    }

    /// Ordered shutdown: display restore first (if HDR was managed), then the
    /// overlay (and its frame-timing session) and the UI, then the engine.
    fn shutdown(mut self) {
        self.hdr.shutdown();
        self.overlay.stop();
        let matched = self.ui.close();
        log!("record: requested WM_CLOSE for {} Recall UI window(s)", matched.len());
        self.hotkeys = None;
        self.supervisor.shutdown(crate::supervisor::QUIT_GRACE);
        log!("record: clean shutdown (pipe lines dropped for stalled clients: {})", self.hub.dropped());
    }
}

/// Apply the CPU mask + BelowNormal priority to this process (children inherit
/// both at creation). An invalid or unsupported mask is logged, never fatal.
/// Returns the mask (hex text) now in effect, if any.
pub(crate) fn apply_niceness(mask_text: &str) -> Option<String> {
    let mask = parse_cpu_mask(mask_text).unwrap_or_else(|e| {
        log!("record: {e}; using 0x{FALLBACK_CPU_MASK:x}");
        FALLBACK_CPU_MASK
    });
    let _ = unsafe { SetPriorityClass(GetCurrentProcess(), BELOW_NORMAL_PRIORITY_CLASS) };
    apply_affinity(mask)
}

/// Pin this process to `mask`; returns its hex text on success.
pub(crate) fn apply_affinity(mask: usize) -> Option<String> {
    match unsafe { SetProcessAffinityMask(GetCurrentProcess(), mask) } {
        Ok(()) => {
            log!("record: affinity=0x{mask:x}, priority BelowNormal");
            Some(format!("{mask:x}"))
        }
        Err(e) => {
            log!("record: affinity 0x{mask:x} failed ({e}); running unpinned");
            None
        }
    }
}

/// `Start-Recall.ps1 -Valorant`: `RECALL_CAPTURE_HWND` pins one window (name
/// from `RECALL_CAPTURE_NAME`, audio pid from `RECALL_AUDIO_GAME_PID`) until
/// its process exits.
fn pinned_target_from_env() -> Option<Target> {
    let raw = std::env::var("RECALL_CAPTURE_HWND").ok()?;
    let Ok(hwnd) = raw.trim().parse::<usize>() else {
        log!("record: RECALL_CAPTURE_HWND={raw:?} is not a window handle; ignoring the pin");
        return None;
    };
    let hwnd = hwnd as isize;
    let Some((window_pid, stem, exe, title)) = watcher::window_identity(hwnd) else {
        log!("record: RECALL_CAPTURE_HWND={hwnd} is not a live window; ignoring the pin");
        return None;
    };
    let pid = std::env::var("RECALL_AUDIO_GAME_PID")
        .ok()
        .and_then(|v| v.trim().parse::<u32>().ok())
        .filter(|p| *p != 0)
        .unwrap_or(window_pid);
    let name = std::env::var("RECALL_CAPTURE_NAME")
        .ok()
        .map(|n| sanitize(&n))
        .filter(|n| !n.is_empty())
        .unwrap_or_else(|| display_name(&title, &stem));
    log!("record: pinned target from environment: {name:?} ({exe}, pid {pid})");
    Some(Target { hwnd, pid, name, exe })
}
