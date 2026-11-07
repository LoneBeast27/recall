//! Thin Win32 glue for `record_session_end` (U6) and `record_lock` (U8): one hidden window
//! on the pump thread receives `WM_QUERYENDSESSION` / `WM_ENDSESSION` and, registered with
//! `WTSRegisterSessionNotification`, the session lock notices.
//!
//! The window is a hidden top-level one, not a message-only (`HWND_MESSAGE`) window: Windows
//! sends the session-end messages only to top-level windows. It is never shown (a
//! tool window, no `WS_VISIBLE`), so no target scan or UI lookup sees it.
//!
//! The session-end messages need an answer before the handler returns, so the handler acts
//! on the controller directly: the pump's message drain lends it (`drain_messages`), and the
//! handler takes it out while in use, so a nested message cannot reach it twice. A
//! session-end message that arrives outside the drain is queued instead (a query is
//! answered at once; the stop then goes out on the next pump pass, best-effort). Lock
//! notices are always queued for the pump. What each message does is
//! `record_session_host`; this file adds the Win32 calls and the controller as its host.

use std::cell::Cell;
use std::sync::atomic::{AtomicIsize, Ordering};
use std::sync::{Mutex, MutexGuard};
use std::time::{Duration, Instant};

use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, RegisterClassW, WNDCLASSW, WS_EX_TOOLWINDOW, WS_POPUP,
};

use crate::log;
use crate::record_app::Controller;
use crate::record_quit::QuitReason;
use crate::record_session_end::{RegisterRetry, SessionMsg};
use crate::record_session_host::{self as host, SessionHost, SessionState};

/// `NOTIFY_FOR_THIS_SESSION`.
const NOTIFY_FOR_THIS_SESSION: u32 = 0;

#[link(name = "user32")]
extern "system" {
    fn ShutdownBlockReasonCreate(hwnd: HWND, reason: PCWSTR) -> i32;
    fn ShutdownBlockReasonDestroy(hwnd: HWND) -> i32;
}

#[link(name = "wtsapi32")]
extern "system" {
    fn WTSRegisterSessionNotification(hwnd: HWND, flags: u32) -> i32;
    fn WTSUnRegisterSessionNotification(hwnd: HWND) -> i32;
}

thread_local! {
    /// The controller, lent by `drain_messages` for the length of one drain.
    static CONTROLLER: Cell<*mut Controller> = const { Cell::new(std::ptr::null_mut()) };
}

/// Messages for the pump (lock notices; session-end messages nobody could handle in place).
static PENDING: Mutex<Vec<SessionMsg>> = Mutex::new(Vec::new());
/// The hidden window (0 = none).
static WINDOW: AtomicIsize = AtomicIsize::new(0);
/// Retries of a lock-notice registration that failed at start-up.
static REGISTER_RETRY: Mutex<RegisterRetry> = Mutex::new(RegisterRetry::NONE);

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}

fn window() -> Option<HWND> {
    let raw = WINDOW.load(Ordering::Acquire);
    (raw != 0).then_some(HWND(raw as *mut _))
}

/// Create the hidden window and listen for lock notices. On the pump thread, at start-up.
pub(crate) fn start() {
    if window().is_some() {
        return;
    }
    let Ok(instance) = (unsafe { GetModuleHandleW(None) }) else {
        return log!("record: session-end and lock notices unavailable (no module handle)");
    };
    let class_name = w!("RecallSessionWatch");
    let class = WNDCLASSW { lpfnWndProc: Some(session_proc), hInstance: instance.into(), lpszClassName: class_name, ..Default::default() };
    unsafe { RegisterClassW(&class) }; // 0 on a second start: the class is already there
    let created = unsafe {
        CreateWindowExW(WS_EX_TOOLWINDOW, class_name, w!("Recall session watch"), WS_POPUP, 0, 0, 0, 0, None, None, Some(instance.into()), None)
    };
    let hwnd = match created {
        Ok(hwnd) => hwnd,
        Err(e) => return log!("record: session-end and lock notices unavailable ({e}); a shutdown leaves the recording to recovery"),
    };
    WINDOW.store(hwnd.0 as isize, Ordering::Release);
    if register(hwnd) {
        log!("record: listening for sign-out, shutdown and lock");
    } else {
        log!("record: lock notices unavailable ({}); listening for sign-out and shutdown, retrying", std::io::Error::last_os_error());
        *lock(&REGISTER_RETRY) = RegisterRetry::failed_at(Instant::now());
    }
}

fn register(hwnd: HWND) -> bool {
    unsafe { WTSRegisterSessionNotification(hwnd, NOTIFY_FOR_THIS_SESSION) != 0 }
}

/// Pump step: a lock-notice registration that failed at boot (the WTS service not up yet)
/// is tried again every 10 s, up to 5 times.
fn retry_register(now: Instant) {
    let mut retry = lock(&REGISTER_RETRY);
    let Some(hwnd) = window().filter(|_| retry.due(now)) else { return };
    let ok = register(hwnd);
    let error = std::io::Error::last_os_error();
    retry.tried(ok, now);
    match (ok, retry.due(now + crate::record_session_end::REGISTER_RETRY_EVERY)) {
        (true, _) => log!("record: listening for lock notices (retry)"),
        (false, true) => log!("record: lock notices still unavailable ({error}); retrying"),
        (false, false) => log!("record: lock notices unavailable ({error}); gave up"),
    }
}

/// Stop listening and destroy the window. At controller shutdown; safe to call again.
pub(crate) fn stop() {
    let Some(hwnd) = window() else { return };
    WINDOW.store(0, Ordering::Release);
    *lock(&REGISTER_RETRY) = RegisterRetry::NONE;
    unsafe {
        let _ = WTSUnRegisterSessionNotification(hwnd);
        let _ = DestroyWindow(hwnd);
    }
}

/// The pump's message drain, with the controller lent to the session-end handler.
pub(crate) fn drain_messages(ctl: &mut Controller) -> bool {
    let ptr: *mut Controller = ctl;
    CONTROLLER.with(|c| c.set(ptr));
    let quit = crate::app::drain_messages();
    CONTROLLER.with(|c| c.set(std::ptr::null_mut()));
    quit
}

/// Run `f` on the lent controller; `None` when none is lent (or it is in use already).
fn with_controller<R>(f: impl FnOnce(Option<&mut Controller>) -> R) -> R {
    let ptr = CONTROLLER.with(|c| c.replace(std::ptr::null_mut()));
    if ptr.is_null() {
        return f(None);
    }
    // SAFETY: lent by `drain_messages` from the pump's `&mut Controller`, which is not used
    // until the drain returns; taken out while `f` runs, so nothing else can reach it.
    let out = f(Some(unsafe { &mut *ptr }));
    CONTROLLER.with(|c| c.set(ptr));
    out
}

unsafe extern "system" fn session_proc(hwnd: HWND, message: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    let mut queued = Vec::new();
    let answer = with_controller(|ctl| match ctl {
        Some(ctl) => host::dispatch(Some(&mut WinHost { ctl, hwnd: Some(hwnd) }), message, wparam.0, &mut queued),
        None => host::dispatch::<WinHost>(None, message, wparam.0, &mut queued),
    });
    lock(&PENDING).extend(queued);
    match answer {
        Some(answer) => LRESULT(answer),
        None => unsafe { DefWindowProcW(hwnd, message, wparam, lparam) },
    }
}

/// The controller as the session messages' host; `hwnd` for the shutdown block reason.
struct WinHost<'a> {
    ctl: &'a mut Controller,
    hwnd: Option<HWND>,
}

impl SessionHost for WinHost<'_> {
    fn recording(&self) -> bool {
        self.ctl.activity.recording_or_starting()
    }

    fn send_stop(&mut self) {
        let sent = self.ctl.supervisor.send_engine_line(r#"{"cmd":"record_stop"}"#);
        if !crate::record_dispatch::automatic_effects(sent).is_empty() {
            log!("record: session end -- record_stop not sent ({})", crate::record_dispatch::NOT_RESPONDING);
        }
    }

    fn start_recording(&mut self) -> bool {
        self.ctl.engine_command("record_start", None);
        self.ctl.activity.recording_or_starting()
    }

    fn drain_engine(&mut self) {
        self.ctl.drain_engine_events(Instant::now());
    }

    fn block(&mut self, reason: &str) {
        let Some(hwnd) = self.hwnd else { return };
        let reason: Vec<u16> = reason.encode_utf16().chain(Some(0)).collect();
        let _ = unsafe { ShutdownBlockReasonCreate(hwnd, PCWSTR(reason.as_ptr())) };
    }

    fn unblock(&mut self) {
        if let Some(hwnd) = self.hwnd {
            let _ = unsafe { ShutdownBlockReasonDestroy(hwnd) };
        }
    }

    fn shutdown(&mut self, grace: Duration) {
        self.ctl.quit.get_or_insert(QuitReason::SessionEnd);
        self.ctl.shutdown_now(grace);
    }

    fn tell_engine(&mut self, line: &str) {
        let _ = self.ctl.supervisor.send_engine_line(line);
    }

    fn notice(&mut self, title: &str, detail: &str, app_event: &str) {
        self.ctl.hub.event("session", app_event);
        self.ctl.shortcut_notice(title, detail);
    }

    fn now(&self) -> Instant {
        Instant::now()
    }

    fn pause(&mut self, d: Duration) {
        std::thread::sleep(d);
    }

    fn state(&mut self) -> &mut SessionState {
        &mut self.ctl.session_watch
    }
}

impl Controller {
    /// Pump step: the lock notices (and any session-end message the window could not
    /// handle in place) since the last pass; a failed lock registration's retries.
    pub(crate) fn session_tick(&mut self, now: Instant) {
        retry_register(now);
        let pending = std::mem::take(&mut *lock(&PENDING));
        host::session_tick(&mut WinHost { ctl: self, hwnd: window() }, pending, now);
    }

    /// A fresh engine learns that the session is locked (it counts from now).
    pub(crate) fn reapply_lock(&mut self) {
        host::reapply_lock(&mut WinHost { ctl: self, hwnd: None });
    }
}
