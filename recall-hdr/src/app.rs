//! The legacy HDR display-manager tray (`--legacy-hdr`): single Win32 message
//! pump wiring watcher + tray + hotkey + supervisor + reconciler + journal/guards
//! into the run loop.
//!
//! # The one pump thread
//! `Tray::build`, `Hotkey::register`, and `Watcher::install` all bind to the
//! thread that owns the Win32 message pump, and their events arrive on
//! process-global receivers. So this whole module runs on ONE thread (the thread
//! that calls [`run`]): we build exactly one `Tray`, one `Hotkey`, one `Watcher`,
//! then loop `MsgWaitForMultipleObjectsEx` -> drain `PeekMessageW` (drives the
//! WinEvent callback + tray menu + hotkey) -> drain our channels/pollers ->
//! `reconciler.poll(now)` -> execute actions.
//!
//! # The "display is never left stuck" spine
//! The journalled executor, the restore spine and the startup repair live in
//! [`crate::hdr_exec`] (shared with the unified record mode's optional HDR
//! manager); process liveness lives in [`crate::liveness`].

use std::sync::mpsc::{channel, Receiver};
use std::time::{Duration, Instant};

use windows::Win32::Foundation::POINT;
use windows::Win32::Graphics::Gdi::{MonitorFromPoint, HMONITOR, MONITOR_DEFAULTTOPRIMARY};
use windows::Win32::UI::WindowsAndMessaging::{
    DispatchMessageW, MsgWaitForMultipleObjectsEx, PeekMessageW, TranslateMessage, MSG,
    MWMO_INPUTAVAILABLE, PM_REMOVE, QS_ALLINPUT, WM_QUIT,
};

use crate::config::{Config, Override};
use crate::control::{Inbound, PipeHub};
use crate::display;
use crate::guard::OnceRestore;
use crate::hdr_exec::{self, HdrExecutor};
use crate::liveness::ProcessWatch;
use crate::log;
use crate::rules::{self, Foreground};
use crate::state::Reconciler;
use crate::supervisor::{EngineHealth, Supervisor};
use crate::tray::{self, Tray, TrayCmd};
use crate::types::RuleKind;
use crate::watcher::{WatchEvent, Watcher};

/// Cap on the pump wait so `poll()` stays lively even with no pending deadline
/// (session-liveness + supervisor-health checks want to tick regularly).
const MAX_WAIT_MS: u32 = 250;

// ===========================================================================
// App state
// ===========================================================================

/// The live application, owned entirely by [`run`] on the pump thread.
struct App {
    cfg: Config,
    reconciler: Reconciler,
    /// Journalled display executor (target + journal + clamp + failure caps).
    exec: HdrExecutor,

    /// Master auto-enable switch (tray "Auto HDR/SDR by game"). When false we still
    /// drain the watcher channel but do not feed `on_foreground`.
    auto_enabled: bool,
    /// The last non-denied foreground stem, for FocusedRule / ForgetFocusedRule.
    last_stem: Option<String>,

    /// Liveness of the reconciler's session pid.
    liveness: ProcessWatch,

    /// Cached status line so we only push tray updates on change.
    last_status: String,

    /// Debounce for the save-clip hotkey (R10: re-presses must not dupe clips).
    last_save_press: Option<Instant>,
}

// ===========================================================================
// Entry point
// ===========================================================================

/// Run the tray application. Blocks on the message pump until Quit / WM_QUIT.
/// Returns a process exit code.
pub fn run() -> i32 {
    let cfg = Config::load_or_default();

    // 0. Startup repair: a prior hard kill may have left the display dirty.
    hdr_exec::repair_if_dirty(&cfg);

    // 1. Resolve the managed display + its monitor (for the watcher's fullscreen
    //    attribution). The PG27AQDP is the primary; use the primary HMONITOR.
    let target = match display::resolve_target(&cfg.display.target_friendly_name) {
        Ok(t) => t,
        Err(e) => {
            log!("app: FATAL -- cannot resolve managed display '{}': {e}", cfg.display.target_friendly_name);
            return 1;
        }
    };
    log!("app: managed display resolved: {}", target.name());
    let target_monitor = primary_monitor_isize();

    // 2. The restore spine: one OnceRestore wrapping a real AppRestore, wired to
    //    the drop-guard AND the ctrl handler. Built before anything mutates.
    let (mut restore_guard, run_restore) = match hdr_exec::install_restore_spine(&cfg) {
        Ok(pair) => pair,
        Err(e) => {
            log!("app: FATAL -- cannot install restore spine: {e}");
            return 1;
        }
    };

    // 3. Build the pump-thread components (exactly one of each).
    let mut tray = match Tray::build(&cfg) {
        Ok(t) => t,
        Err(e) => {
            log!("app: FATAL -- tray build failed: {e}");
            return 1;
        }
    };
    let hotkey = crate::hotkey::Hotkey::register(&cfg.hotkey.toggle_hdr, Some(&cfg.hotkey.save_clip))
        .map_err(|e| log!("app: hotkey register failed ({e}); continuing without a global hotkey"))
        .ok()
        .map(|(hk, save_warn)| {
            if let Some(w) = save_warn {
                log!("app: save-clip hotkey unavailable ({w}); HDR toggle still active");
            }
            hk
        });

    let (tx, watch_rx) = channel::<WatchEvent>();
    let watcher = match Watcher::install(tx, target_monitor) {
        Ok(w) => Some(w),
        Err(e) => {
            log!("app: watcher install failed ({e}); auto HDR/SDR will not fire");
            None
        }
    };

    // 4. The supervised capture engine + the hub control pipe.
    let supervisor = Supervisor::start();
    let (ctl_tx, ctl_rx) = channel::<Inbound>();
    let control_hub = PipeHub::start(ctl_tx);
    supervisor.attach_hub(control_hub.clone());

    // 5. App state.
    let mut app = App {
        reconciler: Reconciler::new(
            Duration::from_millis(cfg.behavior.debounce_ms as u64),
            cfg.behavior.sdr_white_nits,
        ),
        exec: HdrExecutor::new(target),
        auto_enabled: true,
        last_stem: None,
        liveness: ProcessWatch::new("app: session"),
        last_status: String::new(),
        last_save_press: None,
        cfg,
    };

    log!("app: entering message pump");
    let code = app.pump(&mut tray, hotkey.as_ref(), &watch_rx, &supervisor, &run_restore, &control_hub, &ctl_rx);

    // 7. Shutdown ordering: watcher off first, then restore, then engine, then
    //    journal.clear (if clean), then disarm the guard.
    if let Some(w) = watcher {
        w.uninstall(); // Drop -> UnhookWinEvent on this (install) thread.
    }
    run_restore.run(); // idempotent: display back to baseline via the OnceRestore.
    supervisor.shutdown(Duration::from_secs(3));
    if crate::journal::Journal::is_dirty() {
        if let Err(e) = app.exec.clear_journal() {
            log!("app: journal clear on shutdown failed: {e}");
        }
    }
    restore_guard.disarm(); // clean path already restored; don't re-run on drop.
    log!("app: shutdown complete");
    code
}

// ===========================================================================
// The pump loop
// ===========================================================================

impl App {
    /// The single Win32 message pump. Returns the process exit code.
    #[allow(clippy::too_many_arguments)]
    fn pump(
        &mut self,
        tray: &mut Tray,
        hotkey: Option<&crate::hotkey::Hotkey>,
        watch_rx: &Receiver<WatchEvent>,
        supervisor: &Supervisor,
        run_restore: &std::sync::Arc<OnceRestore>,
        hub: &std::sync::Arc<PipeHub>,
        ctl_rx: &Receiver<Inbound>,
    ) -> i32 {
        loop {
            // (a) Wait until a deadline, a message, or the wait cap.
            let timeout = self.wait_timeout_ms();
            unsafe {
                MsgWaitForMultipleObjectsEx(None, timeout, QS_ALLINPUT, MWMO_INPUTAVAILABLE);
            }

            // (b) Drain the message queue: drives the WinEvent hook callback
            //     (watcher), the tray context menu, and the hotkey.
            if drain_messages() {
                log!("app: WM_QUIT received");
                return 0;
            }

            // (c) Watcher events -> reconciler.
            self.drain_watcher(watch_rx);

            // (d) Tray commands. Quit ends the loop cleanly.
            for cmd in tray.poll() {
                if self.handle_tray_cmd(cmd, tray, run_restore) {
                    return 0;
                }
            }

            // (e) Hotkeys fired -> HDR toggle / replay save.
            let fired = hotkey.map(|h| h.poll()).unwrap_or_default();
            if fired.toggle_hdr {
                self.toggle_hdr_now();
            }
            if fired.save_clip {
                self.save_clip_now(supervisor);
            }

            // (e2) Hub control-pipe commands (executed here, on the pump thread).
            while let Ok(inbound) = ctl_rx.try_recv() {
                self.handle_control_cmd(&inbound.line, supervisor, hub);
            }

            // (f) Session liveness: has the tracked game process exited?
            if let Some(pid) = self.liveness.poll(self.reconciler.session_pid()) {
                self.reconciler.on_session_exit(pid, Instant::now());
            }

            // (g) Reconcile: emit + execute the converging actions.
            let now = Instant::now();
            for action in self.reconciler.poll(now) {
                self.exec.execute(action, &mut self.reconciler);
            }

            // (h) Status line + tray checks (cheap; only on change).
            self.update_status(tray, supervisor, Some(hub));
        }
    }

    /// Timeout for the next pump wait: the reconciler's pending deadline (clamped
    /// to [`MAX_WAIT_MS`]), or the cap when nothing is pending.
    fn wait_timeout_ms(&self) -> u32 {
        wait_ms(self.reconciler.next_deadline(), MAX_WAIT_MS)
    }

    /// Convert queued [`WatchEvent`]s to [`Foreground`], classify, and feed the
    /// reconciler (unless auto is disabled). Records `last_stem` for the tray's
    /// FocusedRule actions. The watcher does NOT lowercase the stem; we do.
    fn drain_watcher(&mut self, watch_rx: &Receiver<WatchEvent>) {
        while let Ok(ev) = watch_rx.try_recv() {
            let fg = foreground_of(&ev);
            let kind = rules::classify(&fg, &self.cfg);
            // Remember the last focused stem so FocusedRule targets the right one
            // (deny-listed stems are harmless to store: classify ignores them).
            self.last_stem = Some(fg.exe_stem.clone());
            if !self.auto_enabled {
                continue; // still drained the channel; just don't act on it.
            }
            // Clamp mode for an SDR game comes from the per-exe override or default.
            let clamp = self.cfg.rule_for(&fg.exe_stem).1;
            self.reconciler.on_foreground(kind, ev.pid, clamp, Instant::now());
        }
    }

    // -- tray commands ------------------------------------------------------

    /// Handle a tray command. Returns `true` if it was Quit (end the loop).
    fn handle_tray_cmd(&mut self, cmd: TrayCmd, tray: &mut Tray, run_restore: &std::sync::Arc<OnceRestore>) -> bool {
        match cmd {
            TrayCmd::Quit => return true,
            TrayCmd::ToggleHdrNow => self.toggle_hdr_now(),
            TrayCmd::SetAutoEnabled(on) => {
                self.auto_enabled = on;
                log!("app: auto HDR/SDR by game = {on}");
            }
            TrayCmd::FocusedRule(kind) => self.focused_rule(kind),
            TrayCmd::ForgetFocusedRule => self.forget_focused_rule(),
            TrayCmd::SetDefaultClamp(mode) => {
                self.cfg.behavior.default_clamp = mode;
                self.save_config();
            }
            TrayCmd::SetAutostart(on) => {
                self.cfg.startup.autostart = on;
                self.save_config(); // Config::save syncs the HKCU Run key.
            }
            TrayCmd::OpenConfig => {
                let _ = tray::open_path(&Config::path());
            }
            TrayCmd::OpenLog => {
                let _ = tray::open_path(&log_file_path());
            }
            TrayCmd::RestoreNow => self.restore_now(run_restore),
        }
        // Reflect any config-driven check changes back into the tray.
        tray.set_checks(self.auto_enabled, self.cfg.startup.autostart, self.cfg.behavior.default_clamp);
        false
    }

    /// Manual HDR flip (tray "Toggle HDR now" / Ctrl+Alt+H): set a reconciler-level
    /// desired-state PIN, not a direct executor call, so the next poll applies it
    /// through the journalled path and does not diff it back out (the ~290ms
    /// self-cancel bug). A live game session still outranks the pin.
    fn toggle_hdr_now(&mut self) {
        let on = self.reconciler.toggle_manual_hdr();
        log!("app: manual HDR pin -> {on}");
    }

    /// Save-clip hotkey: debounce re-presses (2s) then forward `save_clip` to
    /// the supervised engine over its stdin protocol.
    fn save_clip_now(&mut self, supervisor: &Supervisor) {
        let now = Instant::now();
        if self.last_save_press.is_some_and(|t| now.duration_since(t) < Duration::from_secs(2)) {
            log!("app: save_clip hotkey debounced");
            return;
        }
        self.last_save_press = Some(now);
        log!("app: save_clip hotkey -> engine");
        supervisor.send_save_clip();
    }

    /// Upsert an override for the last focused stem to `kind`, then save.
    fn focused_rule(&mut self, kind: RuleKind) {
        let Some(stem) = self.last_stem.clone() else {
            log!("app: FocusedRule({kind:?}) ignored -- no focused app recorded yet");
            return;
        };
        if let Some(o) = self.cfg.overrides.iter_mut().find(|o| o.exe.eq_ignore_ascii_case(&stem)) {
            o.kind = kind;
        } else {
            self.cfg.overrides.push(Override { exe: stem.clone(), kind, clamp: None });
        }
        log!("app: focused rule set: {stem} -> {kind:?}");
        self.save_config();
    }

    /// Remove the override for the last focused stem, then save.
    fn forget_focused_rule(&mut self) {
        let Some(stem) = self.last_stem.clone() else {
            log!("app: ForgetFocusedRule ignored -- no focused app recorded yet");
            return;
        };
        let before = self.cfg.overrides.len();
        self.cfg.overrides.retain(|o| !o.exe.eq_ignore_ascii_case(&stem));
        if self.cfg.overrides.len() != before {
            log!("app: forgot rule for {stem}");
            self.save_config();
        } else {
            log!("app: no rule to forget for {stem}");
        }
    }

    /// RestoreNow: run the restore ops (back to baseline) and reset the reconciler
    /// so it re-diffs from a clean baseline.
    fn restore_now(&mut self, run_restore: &std::sync::Arc<OnceRestore>) {
        log!("app: RestoreNow");
        // Drop any manual HDR pin FIRST so the reconciler (recreated below) does not
        // immediately re-assert HDR after we restore to baseline.
        self.reconciler.clear_manual();
        hdr_exec::restore_now(&self.cfg);
        let _ = run_restore; // once-cell reserved for the terminal restore path.
        self.reconciler = Reconciler::new(
            Duration::from_millis(self.cfg.behavior.debounce_ms as u64),
            self.cfg.behavior.sdr_white_nits,
        );
        self.liveness.close();
        self.exec.reset_failures();
    }

    fn save_config(&self) {
        if let Err(e) = self.cfg.save() {
            log!("app: config save failed: {e}");
        }
    }

    // -- control pipe ---------------------------------------------------------

    /// Execute one hub command line ON THE PUMP THREAD (display state is
    /// single-threaded by design; the pipe threads only queue lines).
    fn handle_control_cmd(&mut self, line: &str, supervisor: &Supervisor, hub: &std::sync::Arc<PipeHub>) {
        let Ok(val) = serde_json::from_str::<serde_json::Value>(line) else {
            return;
        };
        match val.get("cmd").and_then(|c| c.as_str()) {
            Some("save_clip") => {
                log!("app: control save_clip -> engine");
                match val.get("seconds").and_then(|s| s.as_u64()) {
                    Some(s) => supervisor.send_engine_line(&format!(r#"{{"cmd":"save_clip","seconds":{s}}}"#)),
                    None => supervisor.send_save_clip(),
                }
            }
            Some("export_clip") => {
                if matches!(supervisor.health(), EngineHealth::Running { .. }) {
                    supervisor.send_engine_line(&val.to_string());
                } else {
                    hub.event("engine", &serde_json::json!({
                        "type": "export_failed", "source": val.get("path").and_then(|v| v.as_str()).unwrap_or(""),
                        "reason": "recording engine is offline; start Recall before exporting"
                    }).to_string());
                }
            }
            Some("toggle_hdr") => {
                log!("app: control toggle_hdr");
                self.toggle_hdr_now();
            }
            Some("set_mode") => {
                if let Some(v) = val.get("value").and_then(|v| v.as_str()) {
                    if matches!(v, "auto" | "sdr" | "hdr") {
                        supervisor.send_engine_line(&format!(r#"{{"cmd":"set_mode","value":"{v}"}}"#));
                    }
                }
            }
            Some("status") => {
                // On-demand snapshot for a client that just connected.
                hub.event("hdr", &self.hdr_status_json(supervisor));
                for line in supervisor.cached_engine_events() {
                    hub.event("engine", &line);
                }
            }
            Some("set_ac_profile") => {
                let variant = val.get("variant").and_then(|v| v.as_str()).unwrap_or("");
                let Some(profile) = hdr_exec::ac_profile_file(variant) else {
                    log!("app: control set_ac_profile -- unknown variant");
                    return;
                };
                let target = self.cfg.display.target_friendly_name.clone();
                let result = crate::wcs::set_advanced_color_default(&target, profile).map(|()| profile);
                match &result {
                    Ok(_) => log!("app: control set_ac_profile -> {profile} (next HDR entry)"),
                    Err(e) => log!("app: control set_ac_profile FAILED: {e}"),
                }
                hub.event("hdr", &hdr_exec::ac_profile_event(&result));
            }
            _ => log!("app: control -- unknown command line ignored"),
        }
    }

    /// Structured HDR/daemon state for hub clients.
    fn hdr_status_json(&self, supervisor: &Supervisor) -> String {
        serde_json::json!({
            "type": "status",
            "target": self.exec.target().name(),
            "hdr": self.reconciler.actual().hdr,
            "manual_pin": self.reconciler.manual_pinned(),
            "session_pid": self.reconciler.session_pid(),
            "auto": self.auto_enabled,
            "engine_health": health_summary(supervisor.health()),
        })
        .to_string()
    }

    // -- status -------------------------------------------------------------

    /// Recompute the status line + tray checks and push them only on change.
    fn update_status(&mut self, tray: &mut Tray, supervisor: &Supervisor, hub: Option<&std::sync::Arc<PipeHub>>) {
        let actual = self.reconciler.actual();
        let session_desc = match self.reconciler.session_pid() {
            Some(pid) => format!("game (pid {pid})"),
            None => "desktop".to_string(),
        };
        let health = health_summary(supervisor.health());
        // Reflect a manual pin so the user sees WHY HDR is on ("HDR (manual)").
        let hdr_label = if actual.hdr {
            if self.reconciler.manual_pinned() && self.reconciler.session_pid().is_none() {
                "HDR (manual)"
            } else {
                "HDR"
            }
        } else {
            "SDR"
        };
        let line = format!("{}: {} · {} · engine {}", self.exec.target().name(), hdr_label, session_desc, health);
        if line != self.last_status {
            tray.set_status(&line);
            tray.set_checks(self.auto_enabled, self.cfg.startup.autostart, self.cfg.behavior.default_clamp);
            self.last_status = line;
            if let Some(hub) = hub {
                hub.event("hdr", &self.hdr_status_json(supervisor));
            }
        }
    }
}

// ===========================================================================
// Free helpers (shared with the unified record mode)
// ===========================================================================

/// Drain the thread message queue with `PeekMessageW`. Returns `true` if a
/// `WM_QUIT` was seen. Dispatching drives the out-of-context WinEvent callback,
/// the tray popup menu, and the global hotkey.
pub(crate) fn drain_messages() -> bool {
    let mut msg = MSG::default();
    loop {
        let got = unsafe { PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE) };
        if !got.as_bool() {
            return false;
        }
        if msg.message == WM_QUIT {
            return true;
        }
        unsafe {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
}

/// Milliseconds until `deadline` (0 if passed), capped at `cap`.
pub(crate) fn wait_ms(deadline: Option<Instant>, cap: u32) -> u32 {
    match deadline {
        Some(d) => d.saturating_duration_since(Instant::now()).as_millis().min(cap as u128) as u32,
        None => cap,
    }
}

/// The WatchEvent -> rule-engine Foreground conversion (stem lowercased).
pub(crate) fn foreground_of(ev: &WatchEvent) -> Foreground {
    Foreground {
        exe_stem: ev.exe_stem.to_ascii_lowercase(),
        title: ev.title.clone(),
        fullscreen_on_target: ev.fullscreen_on_target,
        pid: ev.pid,
    }
}

/// `%APPDATA%\recall-hdr\recall-hdr.log` -- mirrors `logging::log_path` (which is
/// private to that module) for the tray's "Open log" helper.
fn log_file_path() -> std::path::PathBuf {
    let base = std::env::var("APPDATA")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| std::env::temp_dir());
    base.join("recall-hdr").join("recall-hdr.log")
}

/// The primary monitor's `HMONITOR` as an `isize` (the watcher's target format).
/// The managed panel (PG27AQDP) is the primary display on this box.
pub(crate) fn primary_monitor_isize() -> isize {
    let mon: HMONITOR = unsafe { MonitorFromPoint(POINT { x: 0, y: 0 }, MONITOR_DEFAULTTOPRIMARY) };
    mon.0 as isize
}

/// A short, human summary of engine health for the status line.
pub(crate) fn health_summary(h: EngineHealth) -> &'static str {
    match h {
        EngineHealth::Running { .. } => "running",
        EngineHealth::Restarting { .. } => "restarting",
        EngineHealth::GivenUp => "off",
        EngineHealth::Stopped => "stopped",
    }
}

// ===========================================================================
// Tests (pure units only)
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn health_summary_maps_all_variants() {
        assert_eq!(health_summary(EngineHealth::Running { pid: 1 }), "running");
        assert_eq!(health_summary(EngineHealth::Restarting { attempt: 1 }), "restarting");
        assert_eq!(health_summary(EngineHealth::GivenUp), "off");
        assert_eq!(health_summary(EngineHealth::Stopped), "stopped");
    }

    /// The WatchEvent -> Foreground conversion: stem is lowercased, the
    /// managed-monitor fullscreen flag (not the any-monitor one) is carried.
    #[test]
    fn watchevent_to_foreground_lowercases_stem() {
        let ev = WatchEvent {
            pid: 4321,
            exe_stem: "HDR-TestCard".to_string(),
            exe_name: "HDR-TestCard.exe".to_string(),
            title: "Test".to_string(),
            fullscreen: true,
            fullscreen_on_target: false,
            hwnd: 0x1234,
        };
        let fg = foreground_of(&ev);
        assert_eq!(fg.exe_stem, "hdr-testcard", "stem must be lowercased");
        assert_eq!(fg.pid, 4321);
        assert!(!fg.fullscreen_on_target, "HDR rules see only the managed monitor");
        assert_eq!(fg.title, "Test");
    }

    #[test]
    fn wait_ms_clamps_to_cap_and_floors_at_zero() {
        assert_eq!(wait_ms(None, 250), 250);
        assert_eq!(wait_ms(Some(Instant::now() - Duration::from_secs(1)), 250), 0);
        assert_eq!(wait_ms(Some(Instant::now() + Duration::from_secs(60)), 250), 250);
    }
}
