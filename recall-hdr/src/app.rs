//! The owning App: single Win32 message pump wiring watcher + tray + hotkey +
//! supervisor + reconciler + journal/guards into the run loop.
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
//! Every mutation is journalled *before* it happens ([`journal::Journal::record`])
//! and a single [`guard::OnceRestore`] wraps a real [`journal::RestoreOps`] impl
//! ([`AppRestore`]) that is wired to BOTH the RAII drop-guard and the console ctrl
//! handler. Startup repairs a prior hard-kill via [`journal::is_dirty`] +
//! [`journal::restore_with`]. `--restore` (main.rs) drives the same `AppRestore`.

use std::sync::mpsc::{channel, Receiver};
use std::time::{Duration, Instant};

use windows::Win32::Foundation::{CloseHandle, HANDLE, POINT, WAIT_FAILED, WAIT_OBJECT_0};
use windows::Win32::Graphics::Gdi::{MonitorFromPoint, HMONITOR, MONITOR_DEFAULTTOPRIMARY};
use windows::Win32::System::Threading::{
    OpenProcess, WaitForSingleObject, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SYNCHRONIZE,
};
use windows::Win32::UI::WindowsAndMessaging::{
    DispatchMessageW, MsgWaitForMultipleObjectsEx, PeekMessageW, TranslateMessage, MSG,
    MWMO_INPUTAVAILABLE, PM_REMOVE, QS_ALLINPUT, WM_QUIT,
};

use crate::clamp::{Capability, GamutClamp, NvApiClamp, NoopClamp};
use crate::config::{Config, Override};
use crate::display::{self, TargetId};
use crate::guard::{self, OnceRestore, RestoreGuard};
use crate::journal::{self, Journal, RestoreOps};
use crate::log;
use crate::rules::{self, Foreground};
use crate::state::Reconciler;
use crate::supervisor::{EngineHealth, Supervisor};
use crate::tray::{self, Tray, TrayCmd};
use crate::types::{Action, ClampMode, RuleKind};
use crate::watcher::{WatchEvent, Watcher};

/// Cap on the pump wait so `poll()` stays lively even with no pending deadline
/// (session-liveness + supervisor-health checks want to tick regularly).
const MAX_WAIT_MS: u32 = 250;

/// After this many consecutive failures of the *same* action kind, we `confirm`
/// it anyway so the reconcile loop stops re-emitting a doomed action forever.
const MAX_ACTION_FAILURES: u32 = 3;

// ===========================================================================
// RestoreOps: the real display-restore executor (shared by run() and --restore)
// ===========================================================================

/// The concrete [`RestoreOps`] the journal drives on any restore path. Owns a
/// resolved [`TargetId`] and a gamut-clamp backend, so both `app::run()` (clean
/// shutdown, RestoreNow, ctrl-handler, drop-guard) and `--restore` (startup
/// repair) apply an identical restore.
///
/// `pub(crate)` so `main.rs`'s upgraded `--restore` can construct and drive it.
pub(crate) struct AppRestore {
    target: TargetId,
    clamp: Box<dyn GamutClamp + Send>,
}

impl AppRestore {
    /// Build a restore executor. Resolves the managed display by friendly name and
    /// initializes the NvAPI clamp (falling back to a no-op clamp if NvAPI is
    /// unavailable -- a restore must never fail to construct just because the GPU
    /// clamp is absent; HDR/SDR-white restore is adapter-agnostic).
    pub(crate) fn new(cfg: &Config) -> Result<AppRestore, String> {
        let target = display::resolve_target(&cfg.display.target_friendly_name)?;
        let clamp = build_clamp();
        Ok(AppRestore { target, clamp })
    }
}

impl RestoreOps for AppRestore {
    fn set_hdr(&mut self, on: bool) -> Result<(), String> {
        display::set_hdr_state(&self.target, on)
    }
    fn clear_clamp(&mut self) -> Result<(), String> {
        self.clamp.reset()
    }
    fn set_sdr_white(&mut self, nits: u32) -> Result<(), String> {
        display::set_sdr_white(&self.target, nits)
    }
}

/// Construct a clamp backend: the real NvAPI clamp when reachable, else a logged
/// no-op (so the app runs on non-NVIDIA / driverless boxes without special-casing
/// every call site). `Send` so it can live in the ctrl-handler closure.
fn build_clamp() -> Box<dyn GamutClamp + Send> {
    match NvApiClamp::init() {
        Ok(nv) => {
            log!("app: NvAPI clamp initialized");
            Box::new(nv)
        }
        Err(e) => {
            log!("app: NvAPI clamp unavailable ({e}); using no-op clamp (HDR/SDR-white still managed)");
            Box::new(NoopClamp)
        }
    }
}

// ===========================================================================
// Per-action failure tracking (avoid infinite retry storms)
// ===========================================================================

/// Tracks consecutive failures per action *kind* so a persistently-failing action
/// is force-confirmed after [`MAX_ACTION_FAILURES`] rather than re-emitted forever.
#[derive(Default)]
struct FailureCounter {
    enable_hdr: u32,
    disable_hdr: u32,
    set_clamp: u32,
    clear_clamp: u32,
    set_sdr_white: u32,
}

impl FailureCounter {
    /// Bump the counter for `action`'s kind and return the new consecutive count.
    fn bump(&mut self, action: Action) -> u32 {
        let slot = self.slot(action);
        *slot += 1;
        *slot
    }
    /// Reset the counter for `action`'s kind (called on success).
    fn reset(&mut self, action: Action) {
        *self.slot(action) = 0;
    }
    fn slot(&mut self, action: Action) -> &mut u32 {
        match action {
            Action::EnableHdr => &mut self.enable_hdr,
            Action::DisableHdr => &mut self.disable_hdr,
            Action::SetClamp(_) => &mut self.set_clamp,
            Action::ClearClamp => &mut self.clear_clamp,
            Action::SetSdrWhite(_) => &mut self.set_sdr_white,
            // Noop never fails; give it a throwaway slot it never reaches.
            Action::Noop => &mut self.set_sdr_white,
        }
    }
}

// ===========================================================================
// App state
// ===========================================================================

/// The live application, owned entirely by [`run`] on the pump thread.
struct App {
    cfg: Config,
    target: TargetId,
    reconciler: Reconciler,
    journal: Journal,
    /// Executor clamp backend (distinct instance from the restore spine's clamp;
    /// both are cheap handles to the same driver).
    clamp: Box<dyn GamutClamp + Send>,
    failures: FailureCounter,

    /// Master auto-enable switch (tray "Auto HDR/SDR by game"). When false we still
    /// drain the watcher channel but do not feed `on_foreground`.
    auto_enabled: bool,
    /// The last non-denied foreground stem, for FocusedRule / ForgetFocusedRule.
    last_stem: Option<String>,

    /// Kept-open liveness handle for the current session pid (`None` when on the
    /// baseline). Re-opened when the session pid changes.
    session_handle: Option<(u32, HANDLE)>,
    /// Whether we've already logged a `WaitForSingleObject` failure for the
    /// *current* session handle (one line per session, not per 250ms tick).
    /// Reset whenever a new session handle is opened.
    liveness_wait_failed_logged: bool,

    /// Cached status line so we only push tray updates on change.
    last_status: String,
}

// ===========================================================================
// Entry point
// ===========================================================================

/// Run the tray application. Blocks on the message pump until Quit / WM_QUIT.
/// Returns a process exit code.
pub fn run() -> i32 {
    let cfg = Config::load_or_default();

    // 0. Startup repair: a prior hard kill may have left the display dirty.
    if Journal::is_dirty() {
        log!("app: journal is dirty at startup -- repairing a prior hard-kill's display state");
        match AppRestore::new(&cfg) {
            Ok(mut ops) => match journal::restore_with(&mut ops) {
                Ok(true) => log!("app: startup restore complete"),
                Ok(false) => log!("app: startup restore -- nothing to restore"),
                Err(e) => log!("app: startup restore INCOMPLETE ({e}); journal left dirty for retry"),
            },
            Err(e) => log!("app: startup restore skipped -- cannot build restore ops: {e}"),
        }
    }

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
    let (mut restore_guard, run_restore) = match install_restore_spine(&cfg) {
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
    let hotkey = crate::hotkey::Hotkey::register(&cfg.hotkey.toggle_hdr)
        .map_err(|e| log!("app: hotkey register failed ({e}); continuing without a global hotkey"))
        .ok();

    let (tx, watch_rx) = channel::<WatchEvent>();
    let watcher = match Watcher::install(tx, target_monitor) {
        Ok(w) => Some(w),
        Err(e) => {
            log!("app: watcher install failed ({e}); auto HDR/SDR will not fire");
            None
        }
    };

    // 4. The supervised capture engine.
    let supervisor = Supervisor::start();

    // 5. App state.
    let mut app = App {
        reconciler: Reconciler::new(
            Duration::from_millis(cfg.behavior.debounce_ms as u64),
            cfg.behavior.sdr_white_nits,
        ),
        journal: Journal::open(),
        clamp: build_clamp(),
        failures: FailureCounter::default(),
        auto_enabled: true,
        last_stem: None,
        session_handle: None,
        liveness_wait_failed_logged: false,
        last_status: String::new(),
        target,
        cfg,
    };

    log!("app: entering message pump");
    let code = app.pump(&mut tray, hotkey.as_ref(), &watch_rx, &supervisor, &run_restore);

    // 7. Shutdown ordering: watcher off first, then restore, then engine, then
    //    journal.clear (if clean), then disarm the guard.
    if let Some(w) = watcher {
        w.uninstall(); // Drop -> UnhookWinEvent on this (install) thread.
    }
    run_restore.run(); // idempotent: display back to baseline via the OnceRestore.
    supervisor.shutdown(Duration::from_secs(3));
    if !Journal::is_dirty() {
        // Restore either cleared the journal or there was nothing to restore.
    } else if let Err(e) = app.journal.clear() {
        log!("app: journal clear on shutdown failed: {e}");
    }
    restore_guard.disarm(); // clean path already restored; don't re-run on drop.
    log!("app: shutdown complete");
    code
}

/// Build the [`AppRestore`]-backed [`OnceRestore`] and wire it to the RAII guard
/// and the console control handler. Returns the guard (to disarm on clean exit)
/// and a shared `OnceRestore` handle the app runs explicitly on clean shutdown /
/// RestoreNow.
fn install_restore_spine(cfg: &Config) -> Result<(RestoreGuard, std::sync::Arc<OnceRestore>), String> {
    let mut ops = AppRestore::new(cfg)?;
    // The action reads + applies the journal baseline through the real ops. Wrap
    // it so the closure is FnMut + Send (OnceRestore requires Send; ctrl handler
    // needs Send + Sync via install_shared).
    let once = OnceRestore::new(Box::new(move || {
        match journal::restore_with(&mut ops) {
            Ok(true) => log!("app: restore spine applied baseline"),
            Ok(false) => log!("app: restore spine -- nothing to restore"),
            Err(e) => log!("app: restore spine INCOMPLETE ({e}); journal left dirty"),
        }
    }));
    let guard = RestoreGuard::arm_shared(once.clone());
    guard::install_shared(once.clone())?;
    Ok((guard, once))
}

// ===========================================================================
// The pump loop
// ===========================================================================

impl App {
    /// The single Win32 message pump. Returns the process exit code.
    fn pump(
        &mut self,
        tray: &mut Tray,
        hotkey: Option<&crate::hotkey::Hotkey>,
        watch_rx: &Receiver<WatchEvent>,
        supervisor: &Supervisor,
        run_restore: &std::sync::Arc<OnceRestore>,
    ) -> i32 {
        loop {
            // (a) Wait until a deadline, a message, or the wait cap. Cap so polls
            //     stay lively (liveness + health checks tick even when idle).
            let timeout = self.wait_timeout_ms();
            unsafe {
                MsgWaitForMultipleObjectsEx(None, timeout, QS_ALLINPUT, MWMO_INPUTAVAILABLE);
            }

            // (b) Drain the message queue. This is what drives the WinEvent hook
            //     callback (watcher), the tray context menu, and the hotkey.
            if self.drain_messages() {
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

            // (e) Hotkey fired -> same as ToggleHdrNow.
            if hotkey.map(|h| h.poll()).unwrap_or(false) {
                self.toggle_hdr_now();
            }

            // (f) Session liveness: has the tracked game process exited?
            self.check_session_liveness();

            // (g) Reconcile: emit + execute the converging actions.
            let now = Instant::now();
            for action in self.reconciler.poll(now) {
                self.execute(action);
            }

            // (h) Status line + tray checks (cheap; only on change).
            self.update_status(tray, supervisor);
        }
    }

    /// Timeout for the next pump wait: the reconciler's pending deadline (clamped
    /// to [`MAX_WAIT_MS`]), or the cap when nothing is pending.
    fn wait_timeout_ms(&self) -> u32 {
        match self.reconciler.next_deadline() {
            Some(deadline) => {
                let now = Instant::now();
                if deadline <= now {
                    0
                } else {
                    (deadline - now).as_millis().min(MAX_WAIT_MS as u128) as u32
                }
            }
            None => MAX_WAIT_MS,
        }
    }

    /// Drain the thread message queue with `PeekMessageW`. Returns `true` if a
    /// `WM_QUIT` was seen. Dispatching drives the out-of-context WinEvent callback,
    /// the tray popup menu, and the global hotkey.
    fn drain_messages(&self) -> bool {
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

    /// Convert queued [`WatchEvent`]s to [`Foreground`], classify, and feed the
    /// reconciler (unless auto is disabled). Records `last_stem` for the tray's
    /// FocusedRule actions. The watcher does NOT lowercase the stem; we do.
    fn drain_watcher(&mut self, watch_rx: &Receiver<WatchEvent>) {
        while let Ok(ev) = watch_rx.try_recv() {
            let stem = ev.exe_stem.to_ascii_lowercase();
            let fg = Foreground {
                exe_stem: stem.clone(),
                title: ev.title,
                fullscreen_on_target: ev.fullscreen_on_target,
                pid: ev.pid,
            };
            let kind = rules::classify(&fg, &self.cfg);

            // Remember the last app we actually classified as non-denied (a game or
            // an explicit-none), so FocusedRule targets the right stem. A NonGame
            // from the deny-list still updates it only if it wasn't denied -- but we
            // can't cheaply tell here, so track any non-empty stem the user might
            // want to rule on; deny-listed stems are harmless to store (a rule on
            // them is ignored by classify anyway).
            self.last_stem = Some(stem);

            if !self.auto_enabled {
                continue; // still drained the channel; just don't act on it.
            }

            // Clamp mode for an SDR game comes from the per-exe override or default.
            let clamp = self.cfg.rule_for(&fg.exe_stem).1;
            self.reconciler
                .on_foreground(kind, ev.pid, clamp, Instant::now());
        }
    }

    // -- session liveness ---------------------------------------------------

    /// Keep a liveness handle for the reconciler's session pid and, when that
    /// process exits, tell the reconciler. Cheap: one `OpenProcess` per session,
    /// a non-blocking `WaitForSingleObject(0)` per tick.
    fn check_session_liveness(&mut self) {
        let session_pid = self.reconciler.session_pid();

        // Session ended (or replaced): close a stale handle.
        match (&self.session_handle, session_pid) {
            (Some((held, h)), Some(pid)) if *held == pid => {
                // Same session -> poll liveness on the held handle. The handle is
                // opened with SYNCHRONIZE (see `open_session_handle`), so a normal
                // `WaitForSingleObject(0)` returns WAIT_OBJECT_0 once the process
                // exits and WAIT_TIMEOUT while it lives.
                let wait = unsafe { WaitForSingleObject(*h, 0) };
                // FAIL-SAFE: if the wait itself fails (WAIT_FAILED -- e.g. the
                // handle somehow lacks SYNCHRONIZE, or the OS revoked the wait), the
                // liveness instrument can no longer read the process. A liveness
                // probe that can't read MUST NOT strand HDR/clamp on: treat an
                // unreadable session as EXITED so the display always reverts, rather
                // than silently trusting a broken probe forever. Log once per
                // session (this fires every ~250ms tick) so a real regression is
                // visible without flooding the log.
                let exited = wait == WAIT_OBJECT_0;
                let wait_failed = wait == WAIT_FAILED;
                if wait_failed && !self.liveness_wait_failed_logged {
                    self.liveness_wait_failed_logged = true;
                    log!(
                        "app: session pid {pid} liveness WaitForSingleObject failed \
                         ({:?}); treating session as exited (fail-safe -- never strand HDR)",
                        std::io::Error::last_os_error()
                    );
                }
                if exited || wait_failed {
                    if exited {
                        log!("app: session pid {pid} exited");
                    }
                    self.close_session_handle();
                    self.reconciler.on_session_exit(pid, Instant::now());
                }
            }
            (Some(_), _) => {
                // Handle is for a different/no-longer-current pid: drop it, reopen
                // below if there's a current pid.
                self.close_session_handle();
                if let Some(pid) = session_pid {
                    self.open_session_handle(pid);
                }
            }
            (None, Some(pid)) => self.open_session_handle(pid),
            (None, None) => {}
        }
    }

    fn open_session_handle(&mut self, pid: u32) {
        // PROCESS_SYNCHRONIZE is REQUIRED for WaitForSingleObject to signal on exit.
        // With PROCESS_QUERY_LIMITED_INFORMATION alone the wait returns WAIT_FAILED
        // (ACCESS_DENIED), so the exit branch never fires and HDR/clamp is never
        // reverted live. QLI is kept for the process-identity query rights.
        let rights = PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_SYNCHRONIZE;
        self.liveness_wait_failed_logged = false; // fresh session -> fresh log budget
        match unsafe { OpenProcess(rights, false, pid) } {
            Ok(h) if !h.is_invalid() => {
                self.session_handle = Some((pid, h));
            }
            Ok(_) => {}
            Err(e) => {
                // The process may already be gone between commit and here; treat as
                // an immediate exit so we don't strand HDR on a dead game.
                log!("app: OpenProcess({pid}) failed ({e}); treating session as exited");
                self.reconciler.on_session_exit(pid, Instant::now());
            }
        }
    }

    fn close_session_handle(&mut self) {
        if let Some((_, h)) = self.session_handle.take() {
            unsafe {
                let _ = CloseHandle(h);
            }
        }
        self.liveness_wait_failed_logged = false;
    }

    // -- executor -----------------------------------------------------------

    /// Apply one [`Action`] to the hardware with journal-before-mutate, confirming
    /// the reconciler on success (advancing `actual` so the next diff is correct).
    /// On failure we log and, after [`MAX_ACTION_FAILURES`] consecutive failures of
    /// the same action kind, confirm anyway so the loop stops spinning on a doomed
    /// action.
    fn execute(&mut self, action: Action) {
        let result = match action {
            Action::EnableHdr => self.exec_enable_hdr(),
            Action::DisableHdr => self.exec_disable_hdr(),
            Action::SetClamp(mode) => self.exec_set_clamp(mode),
            Action::ClearClamp => self.exec_clear_clamp(),
            Action::SetSdrWhite(nits) => self.exec_set_sdr_white(nits),
            Action::Noop => return,
        };

        match result {
            Ok(()) => {
                self.failures.reset(action);
                self.reconciler.confirm(action);
                // When we return to baseline (nothing left touched), clear the
                // journal so a later clean launch has nothing to repair.
                if !self.journal.baseline().any_touched() {
                    let _ = self.journal.clear();
                }
            }
            Err(e) => {
                let n = self.failures.bump(action);
                log!("app: action {action:?} failed ({e}) [{n}/{MAX_ACTION_FAILURES}]");
                if n >= MAX_ACTION_FAILURES {
                    log!("app: action {action:?} force-confirmed after {n} failures (breaking retry storm)");
                    self.failures.reset(action);
                    self.reconciler.confirm(action);
                }
            }
        }
    }

    fn exec_enable_hdr(&mut self) -> Result<(), String> {
        // Journal the ORIGINAL (pre-first-touch) HDR state before mutating.
        if !self.journal.baseline().hdr_touched {
            let original = display::get_hdr_state(&self.target).unwrap_or(false);
            self.journal.record(|b| {
                b.hdr_touched = true;
                b.hdr_original = original;
            })?;
        }
        display::set_hdr_state(&self.target, true)
    }

    fn exec_disable_hdr(&mut self) -> Result<(), String> {
        // Reconciler semantics: the desktop baseline is HDR-off, so DisableHdr sets
        // false. We still journal that we touched HDR (record original as current
        // if not already recorded) so a crash mid-transition can be repaired.
        if !self.journal.baseline().hdr_touched {
            let original = display::get_hdr_state(&self.target).unwrap_or(false);
            self.journal.record(|b| {
                b.hdr_touched = true;
                b.hdr_original = original;
            })?;
        }
        display::set_hdr_state(&self.target, false)?;

        // Returning to the HDR-OFF baseline un-owes the restore for the facets a
        // live HDR session touched -- WITHOUT this the journal is stranded dirty
        // after every clean live revert (it never auto-clears in `execute`, and a
        // later clean launch runs a spurious startup "repair"). We un-touch AFTER
        // the HDR flip succeeds (mutate-then-journal for the un-touch) so the
        // journal never under-claims a LIVE mutation: a crash before these records
        // just leaves now-moot flags that `--restore` harmlessly re-asserts (the
        // pre-fix behavior, HDR already off / SDR-white a no-op).
        //
        //  * SDR-white is an HDR-MODE property (Windows applies the SDR-content
        //    white level only while HDR is on); with HDR off it owes nothing. This
        //    mirrors the reconciler, whose `confirm(DisableHdr)` resets its
        //    SDR-white model to the "unset" sentinel.
        //  * HDR is un-touched ONLY when we returned it to its recorded original
        //    (`hdr_original == false`, the HDR-off baseline this revert targets).
        //    If the user's original was HDR-ON (`hdr_original == true`) a restore
        //    is still genuinely owed, so we KEEP `hdr_touched` and let the journal
        //    stay dirty for that real repair.
        let base = self.journal.baseline();
        let untouch_hdr = base.hdr_touched && !base.hdr_original;
        if untouch_hdr || base.sdr_white_touched {
            self.journal.record(|b| {
                if untouch_hdr {
                    b.hdr_touched = false;
                }
                b.sdr_white_touched = false;
                b.sdr_white_original = 0;
            })?;
        }
        Ok(())
    }

    fn exec_set_clamp(&mut self, mode: ClampMode) -> Result<(), String> {
        self.journal.record(|b| b.clamp_touched = true)?;
        match self.clamp.apply(mode) {
            Ok(()) => Ok(()),
            Err(e) => {
                // If the clamp capability is genuinely Unavailable, log + succeed so
                // the loop doesn't spin forever on a box without the NvAPI clamp
                // (documented choice: a missing clamp is a degraded mode, not a
                // hard failure that should block reconvergence).
                if let Capability::Unavailable(why) = self.clamp.capability() {
                    log!("app: SetClamp({mode:?}) unavailable ({why}); confirming anyway (degraded)");
                    return Ok(());
                }
                Err(e)
            }
        }
    }

    fn exec_clear_clamp(&mut self) -> Result<(), String> {
        // Clearing returns us toward baseline for the clamp facet.
        self.journal.record(|b| b.clamp_touched = false)?;
        self.clamp.reset()
    }

    fn exec_set_sdr_white(&mut self, nits: u32) -> Result<(), String> {
        if !self.journal.baseline().sdr_white_touched {
            let original = display::get_sdr_white(&self.target).unwrap_or(0);
            self.journal.record(|b| {
                b.sdr_white_touched = true;
                b.sdr_white_original = original;
            })?;
        }
        display::set_sdr_white(&self.target, nits)
    }

    // -- tray commands ------------------------------------------------------

    /// Handle a tray command. Returns `true` if it was Quit (end the loop).
    fn handle_tray_cmd(
        &mut self,
        cmd: TrayCmd,
        tray: &mut Tray,
        run_restore: &std::sync::Arc<OnceRestore>,
    ) -> bool {
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
    /// desired-state PIN, not a direct executor call.
    ///
    /// The pin feeds `desired()`, so the normal poll/executor path applies it
    /// (journalled + confirmed + failure-capped -- all the existing machinery) AND,
    /// critically, the reconciler does NOT diff it back out on the next tick. The old
    /// implementation called `execute(EnableHdr)` directly while `desired()` was still
    /// the HDR-off baseline, so the very next ~250ms poll emitted DisableHdr and the
    /// manual toggle self-cancelled in ~290ms. The pin fixes that at the source.
    /// A live game session still outranks the pin (see `Reconciler::desired`).
    fn toggle_hdr_now(&mut self) {
        let on = self.reconciler.toggle_manual_hdr();
        log!("app: manual HDR pin -> {on}");
        // No direct execute() here: the pump's `reconciler.poll(now)` on this same
        // loop iteration emits the converging actions through the journalled path.
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
    /// so it re-diffs from a clean baseline (recreate it, per the spec).
    fn restore_now(&mut self, run_restore: &std::sync::Arc<OnceRestore>) {
        log!("app: RestoreNow");
        // Drop any manual HDR pin FIRST so the reconciler (recreated below) does not
        // immediately re-assert HDR after we restore to baseline.
        self.reconciler.clear_manual();
        // The shared OnceRestore has already-fired semantics; RestoreNow must be
        // repeatable, so drive a fresh AppRestore directly rather than the once-cell.
        match AppRestore::new(&self.cfg) {
            Ok(mut ops) => {
                let _ = journal::restore_with(&mut ops);
            }
            Err(e) => log!("app: RestoreNow -- cannot build ops: {e}"),
        }
        let _ = run_restore; // once-cell reserved for the terminal restore path.
        // Recreate the reconciler so its `actual` model resets to baseline.
        self.reconciler = Reconciler::new(
            Duration::from_millis(self.cfg.behavior.debounce_ms as u64),
            self.cfg.behavior.sdr_white_nits,
        );
        self.close_session_handle();
        self.failures = FailureCounter::default();
    }

    fn save_config(&self) {
        if let Err(e) = self.cfg.save() {
            log!("app: config save failed: {e}");
        }
    }

    // -- status -------------------------------------------------------------

    /// Recompute the status line + tray checks and push them only on change.
    fn update_status(&mut self, tray: &mut Tray, supervisor: &Supervisor) {
        let actual = self.reconciler.actual();
        let session_desc = match self.reconciler.session_pid() {
            Some(pid) => format!("game (pid {pid})"),
            None => "desktop".to_string(),
        };
        let health = health_summary(supervisor.health());
        // Reflect a manual pin so the user sees WHY HDR is on ("HDR (manual)") when
        // no game is driving it. A game session's own HDR takes the plain "HDR"
        // label (the session, not the pin, owns the display then).
        let hdr_label = if actual.hdr {
            if self.reconciler.manual_pinned() && self.reconciler.session_pid().is_none() {
                "HDR (manual)"
            } else {
                "HDR"
            }
        } else {
            "SDR"
        };
        let line = format!(
            "{}: {} · {} · engine {}",
            self.target.name(),
            hdr_label,
            session_desc,
            health,
        );
        if line != self.last_status {
            tray.set_status(&line);
            tray.set_checks(
                self.auto_enabled,
                self.cfg.startup.autostart,
                self.cfg.behavior.default_clamp,
            );
            self.last_status = line;
        }
    }
}

impl Drop for App {
    fn drop(&mut self) {
        // Never leak the liveness handle.
        self.close_session_handle();
    }
}

// ===========================================================================
// Free helpers
// ===========================================================================

/// `%APPDATA%\recall-hdr\recall-hdr.log` -- mirrors `logging::log_path` (which is
/// private to that module; we can't call it from here, so recompute the same path
/// for the tray's "Open log" helper).
fn log_file_path() -> std::path::PathBuf {
    let base = std::env::var("APPDATA")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| std::env::temp_dir());
    base.join("recall-hdr").join("recall-hdr.log")
}

/// The primary monitor's `HMONITOR` as an `isize` (the watcher's target format).
/// The managed panel (PG27AQDP) is the primary display on this box.
fn primary_monitor_isize() -> isize {
    let mon: HMONITOR =
        unsafe { MonitorFromPoint(POINT { x: 0, y: 0 }, MONITOR_DEFAULTTOPRIMARY) };
    mon.0 as isize
}

/// A short, human summary of engine health for the status line.
fn health_summary(h: EngineHealth) -> &'static str {
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
    fn failure_counter_bumps_and_resets_per_kind() {
        let mut fc = FailureCounter::default();
        assert_eq!(fc.bump(Action::EnableHdr), 1);
        assert_eq!(fc.bump(Action::EnableHdr), 2);
        // A different kind is independent.
        assert_eq!(fc.bump(Action::DisableHdr), 1);
        // SetClamp variants share one slot regardless of mode.
        assert_eq!(fc.bump(Action::SetClamp(ClampMode::Accurate)), 1);
        assert_eq!(fc.bump(Action::SetClamp(ClampMode::Punchy)), 2);
        fc.reset(Action::EnableHdr);
        assert_eq!(fc.bump(Action::EnableHdr), 1, "reset zeroed the EnableHdr slot");
        // DisableHdr untouched by the EnableHdr reset.
        assert_eq!(fc.bump(Action::DisableHdr), 2);
    }

    #[test]
    fn failure_counter_hits_threshold() {
        let mut fc = FailureCounter::default();
        let mut last = 0;
        for _ in 0..MAX_ACTION_FAILURES {
            last = fc.bump(Action::ClearClamp);
        }
        assert_eq!(last, MAX_ACTION_FAILURES, "threshold reached after N bumps");
    }

    #[test]
    fn health_summary_maps_all_variants() {
        assert_eq!(health_summary(EngineHealth::Running { pid: 1 }), "running");
        assert_eq!(health_summary(EngineHealth::Restarting { attempt: 1 }), "restarting");
        assert_eq!(health_summary(EngineHealth::GivenUp), "off");
        assert_eq!(health_summary(EngineHealth::Stopped), "stopped");
    }

    /// The WatchEvent -> Foreground conversion the pump does inline: stem is
    /// lowercased, other load-bearing fields carried through. We replicate the
    /// exact transform here (drain_watcher builds `Foreground` from `WatchEvent`).
    #[test]
    fn watchevent_to_foreground_lowercases_stem() {
        let ev = WatchEvent {
            pid: 4321,
            exe_stem: "HDR-TestCard".to_string(),
            title: "Test".to_string(),
            fullscreen_on_target: true,
            hwnd: 0x1234,
        };
        let stem = ev.exe_stem.to_ascii_lowercase();
        let fg = Foreground {
            exe_stem: stem,
            title: ev.title.clone(),
            fullscreen_on_target: ev.fullscreen_on_target,
            pid: ev.pid,
        };
        assert_eq!(fg.exe_stem, "hdr-testcard", "stem must be lowercased");
        assert_eq!(fg.pid, 4321);
        assert!(fg.fullscreen_on_target);
        assert_eq!(fg.title, "Test");
    }
}
