//! Thin Win32 glue for `record_session_end` (U6) and `record_lock` (U8): one hidden window
//! on the pump thread receives `WM_QUERYENDSESSION` / `WM_ENDSESSION` and, registered with
//! `WTSRegisterSessionNotification`, the session lock notices.
//!
//! The window is a hidden top-level one, not a message-only (`HWND_MESSAGE`) window: Windows
//! sends the session-end messages only to top-level windows. It is never shown (a
//! tool window, no `WS_VISIBLE`), so no target scan or UI lookup sees it.
//!
//! The session-end messages need an answer before the handler returns, so the handler acts
//! on the controller directly: the pump's message drain lends it (`drain_messages`,
//! `record_session_host::Lender`), and the handler takes it out while in use, so a nested
//! message cannot reach it twice. A
//! session-end message that arrives outside the drain is queued instead (a query is
//! answered at once; the stop then goes out on the next pump pass, best-effort). Lock
//! notices are always queued for the pump. What each message does is
//! `record_session_host`; this file adds the Win32 calls and the controller as its host.

use std::sync::atomic::{AtomicBool, AtomicIsize, Ordering};
use std::sync::{Mutex, MutexGuard};
use std::time::{Duration, Instant};

use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{CloseHandle, HWND, LPARAM, LRESULT, WAIT_TIMEOUT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Threading::{OpenEventW, WaitForSingleObject, SYNCHRONIZATION_SYNCHRONIZE};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, RegisterClassW, WNDCLASSW, WS_EX_TOOLWINDOW, WS_POPUP,
};

use crate::log;
use crate::record_app::Controller;
use crate::record_quit::QuitReason;
use crate::record_session_end::{LongState, RegisterRetry, SessionMsg, WM_ENDSESSION, WM_QUERYENDSESSION};
use crate::record_session_save::{self, SessionAction, SessionNotice};
use crate::record_session_host::{self as host, Lender, SessionHost, SessionState};

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
    static CONTROLLER: Lender<Controller> = const { Lender::new() };
}

/// Messages for the pump (lock notices; session-end messages nobody could handle in place).
static PENDING: Mutex<Vec<SessionMsg>> = Mutex::new(Vec::new());
/// The hidden window (0 = none).
static WINDOW: AtomicIsize = AtomicIsize::new(0);
/// The query holds while a replay save is in progress. A long recording has a separate reason.
static SAVE_BLOCKING: AtomicBool = AtomicBool::new(false);
static LONG_BLOCKING: AtomicBool = AtomicBool::new(false);
static SAVE_NOTICES: Mutex<Vec<SessionNotice>> = Mutex::new(Vec::new());
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
    if !terminal_services_ready() && retry.wait_for_service(now) {
        return; // a slow boot: the tries start once the service says it is up (capped)
    }
    let ok = register(hwnd);
    let error = std::io::Error::last_os_error();
    retry.tried(ok, now);
    match (ok, retry.due(now + crate::record_session_end::REGISTER_RETRY_EVERY)) {
        (true, _) => log!("record: listening for lock notices (retry)"),
        (false, true) => log!("record: lock notices still unavailable ({error}); retrying"),
        (false, false) => log!("record: lock notices unavailable ({error}); gave up"),
    }
}

/// `Global\TermSrvReadyEvent` is set (or does not exist): the WTS service takes registrations.
fn terminal_services_ready() -> bool {
    let Ok(event) = (unsafe { OpenEventW(SYNCHRONIZATION_SYNCHRONIZE, false, w!("Global\\TermSrvReadyEvent")) }) else {
        return true;
    };
    let waited = unsafe { WaitForSingleObject(event, 0) };
    let _ = unsafe { CloseHandle(event) };
    waited != WAIT_TIMEOUT
}

fn update_shutdown_reason() {
    let Some(hwnd) = window() else { return };
    let reason = if SAVE_BLOCKING.load(Ordering::Acquire) {
        record_session_save::BLOCK_REASON
    } else if LONG_BLOCKING.load(Ordering::Acquire) {
        crate::record_session_end::BLOCK_REASON
    } else {
        let _ = unsafe { ShutdownBlockReasonDestroy(hwnd) };
        return;
    };
    let reason: Vec<u16> = reason.encode_utf16().chain(Some(0)).collect();
    if unsafe { ShutdownBlockReasonCreate(hwnd, PCWSTR(reason.as_ptr())) } == 0 {
        log!("record: shutdown block reason could not be registered");
    }
}

/// Stop listening and destroy the window. At controller shutdown; safe to call again.
pub(crate) fn stop() {
    let Some(hwnd) = window() else { return };
    SAVE_BLOCKING.store(false, Ordering::Release);
    LONG_BLOCKING.store(false, Ordering::Release);
    lock(&SAVE_NOTICES).clear();
    update_shutdown_reason();
    WINDOW.store(0, Ordering::Release);
    *lock(&REGISTER_RETRY) = RegisterRetry::NONE;
    unsafe {
        let _ = WTSUnRegisterSessionNotification(hwnd);
        let _ = DestroyWindow(hwnd);
    }
}

/// The pump's message drain, with the controller lent to the session-end handler.
pub(crate) fn drain_messages(ctl: &mut Controller) -> bool {
    CONTROLLER.with(|lender| lender.lend(ctl, crate::app::drain_messages))
}

unsafe extern "system" fn session_proc(hwnd: HWND, message: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    let held = message == WM_QUERYENDSESSION && SAVE_BLOCKING.load(Ordering::Acquire);
    if held {
        lock(&SAVE_NOTICES).push(SessionNotice::Query { held });
    } else if message == WM_ENDSESSION && wparam.0 == 0 {
        lock(&SAVE_NOTICES).push(SessionNotice::End { ending: false });
    }
    match CONTROLLER.with(|lender| host::handle(lender, message, wparam.0, &PENDING)) {
        Some(_) if held => LRESULT(isize::from(record_session_save::query_answer(held))),
        Some(answer) => LRESULT(answer),
        None => unsafe { DefWindowProcW(hwnd, message, wparam, lparam) },
    }
}

/// The controller as the session messages' host (the window for the shutdown block reason).
impl SessionHost for Controller {
    fn long_recording(&self) -> bool {
        self.activity.is_recording()
    }

    fn long_state(&self) -> LongState {
        LongState::of(self.activity.recording_or_starting(), self.activity.stop_in_flight())
    }

    fn send_record_stop_now(&mut self) {
        if !crate::record_dispatch::automatic_effects(self.send_record_stop()).is_empty() {
            log!("record: session end -- record_stop not sent ({})", crate::record_dispatch::NOT_RESPONDING);
        }
    }

    fn start_long_recording(&mut self) -> bool {
        let was_idle = !self.activity.recording_or_starting();
        self.engine_command("record_start", None);
        crate::record_session_end::start_went_out(was_idle, self.activity.is_starting())
    }

    fn drain_engine(&mut self) {
        self.drain_engine_events(Instant::now());
    }

    fn block_shutdown(&mut self, _reason: &str) {
        LONG_BLOCKING.store(true, Ordering::Release);
        update_shutdown_reason();
    }

    fn unblock_shutdown(&mut self) {
        LONG_BLOCKING.store(false, Ordering::Release);
        update_shutdown_reason();
    }

    fn session_shutdown(&mut self, grace: Duration) {
        self.quit.get_or_insert(QuitReason::SessionEnd);
        self.shutdown_now(grace);
    }

    fn tell_engine(&mut self, line: &str) {
        let _ = self.supervisor.send_engine_line(line);
    }

    fn session_notice(&mut self, title: &str, detail: &str, app_event: &str) {
        self.hub.event("session", app_event);
        self.shortcut_notice(title, detail);
    }

    fn clock(&self) -> Instant {
        Instant::now()
    }

    fn pause_for(&mut self, d: Duration) {
        std::thread::sleep(d);
    }

    fn session_state(&mut self) -> &mut SessionState {
        &mut self.session_watch
    }
}

impl Controller {
    /// Apply the replay-save hold on the shared session window, even while quitting.
    pub(crate) fn session_end_tick(&mut self, now: Instant) {
        let saving = self.activity.saves_writing(now);
        if let Some(on) = self.block_reason.update(saving) {
            SAVE_BLOCKING.store(on, Ordering::Release);
            update_shutdown_reason();
        }
        let notices = std::mem::take(&mut *lock(&SAVE_NOTICES));
        for notice in notices {
            match self.session_end.on_notice(notice) {
                SessionAction::Quit => {
                    log!("record: the session is ending during a save; finishing it, then quitting");
                    self.quit.get_or_insert(QuitReason::SessionEnd);
                }
                SessionAction::Resume if self.quit == Some(QuitReason::SessionEnd) => {
                    self.quit = crate::record_quit_wait::resume_after_session_end(&mut self.quit_wait);
                }
                SessionAction::Resume | SessionAction::Nothing => {}
            }
        }
    }

    /// Pump step: the lock notices (and any session-end message the window could not
    /// handle in place) since the last pass; a failed lock registration's retries.
    pub(crate) fn session_tick(&mut self, now: Instant) {
        retry_register(now);
        let pending = std::mem::take(&mut *lock(&PENDING));
        host::session_tick(self, pending, now);
    }

    /// A fresh engine learns that the session is locked (it counts from now).
    pub(crate) fn reapply_lock(&mut self) {
        host::reapply_lock(self);
    }
}
