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
//!
//! The hub control-pipe commands live in `app_control.rs`; the free helpers
//! shared with the record mode in `app_helpers.rs`.

#[path = "app_control.rs"]
mod control;
#[path = "app_helpers.rs"]
mod helpers;

use std::sync::mpsc::{channel, Receiver};
use std::time::{Duration, Instant};

use windows::Win32::UI::WindowsAndMessaging::{MsgWaitForMultipleObjectsEx, MWMO_INPUTAVAILABLE, QS_ALLINPUT};

use crate::config::{Config, Override};
use crate::control::{Inbound, PipeHub};
use crate::display;
use crate::guard::OnceRestore;
use crate::hdr_exec::{self, HdrExecutor};
use crate::liveness::ProcessWatch;
use crate::log;
use crate::rules;
use crate::state::Reconciler;
use crate::supervisor::Supervisor;
use crate::tray::{self, Tray, TrayCmd};
use crate::types::RuleKind;
use crate::watcher::{WatchEvent, Watcher};
use helpers::log_file_path;
pub(crate) use helpers::{drain_messages, foreground_of, health_summary, primary_monitor_isize, wait_ms, wait_ms_at};

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
    //    attribution). Attribution uses the primary HMONITOR (the default target).
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
    supervisor.shutdown(crate::supervisor::QUIT_GRACE);
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
            if ev.is_unknown() {
                continue; // an unidentifiable window: the record mode's signal only
            }
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
        if let Some(reason) = crate::record_dispatch::legacy_save(|line| supervisor.send_engine_line(line), None) {
            // The legacy app's hotkey has no notice path of its own: logged, and the failure beep.
            log!("app: save_clip not sent: {reason}");
            crate::supervisor::beep_failure();
        }
    }

    /// Upsert an override for the last focused stem to `kind`, then save.
    fn focused_rule(&mut self, kind: RuleKind) {
        let Some(stem) = self.last_stem.clone() else {
            log!("app: FocusedRule({kind:?}) ignored -- no focused app recorded yet");
            return;
        };
        if focused_rule_refused(&stem, kind) {
            log!("app: FocusedRule({kind:?}) refused -- {stem} is part of Windows or Recall (a game rule also records the app)");
            return;
        }
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
// Tests (pure units only)
// ===========================================================================

#[cfg(test)]
#[path = "app_tests.rs"]
mod tests;

/// R2-detect-r10-5: the legacy tray's "mark focused app HDR/SDR" writes a game override, which
/// also records the app (the user's own entry, F-detect-2): never for Recall or Windows itself
/// (the shared `never_recorded` list). A `none` rule is always fine.
pub(crate) fn focused_rule_refused(stem: &str, kind: RuleKind) -> bool {
    kind != RuleKind::None && crate::never_recorded::never_recordable(stem)
}
