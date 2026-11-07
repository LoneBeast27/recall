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
//! session-end query that arrives outside the drain is queued instead and answered at once
//! (the stop then goes out on the next pump pass, best-effort). Lock notices are always
//! queued for the pump.

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
use crate::record_lock::{app_event_json, engine_line, notice_detail, LockAction, NOTICE_TITLE};
use crate::record_quit::QuitReason;
use crate::record_session_end::{on_end, on_query_end, EndAction, SessionMsg, BLOCK_REASON, QUERY_ANSWER};

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

/// Messages for the pump (lock notices; a session-end query nobody could answer in place).
static PENDING: Mutex<Vec<SessionMsg>> = Mutex::new(Vec::new());
/// The hidden window (0 = none).
static WINDOW: AtomicIsize = AtomicIsize::new(0);

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
    if unsafe { WTSRegisterSessionNotification(hwnd, NOTIFY_FOR_THIS_SESSION) } == 0 {
        log!("record: lock notices unavailable ({}); listening for sign-out and shutdown only", std::io::Error::last_os_error());
    } else {
        log!("record: listening for sign-out, shutdown and lock");
    }
}

/// Stop listening and destroy the window. At controller shutdown; safe to call again.
pub(crate) fn stop() {
    let Some(hwnd) = window() else { return };
    WINDOW.store(0, Ordering::Release);
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

/// Run `f` on the lent controller; false when none is lent (or it is in use already).
fn with_controller(f: impl FnOnce(&mut Controller)) -> bool {
    let ptr = CONTROLLER.with(|c| c.replace(std::ptr::null_mut()));
    if ptr.is_null() {
        return false;
    }
    // SAFETY: lent by `drain_messages` from the pump's `&mut Controller`, which is not used
    // until the drain returns; taken out while `f` runs, so nothing else can reach it.
    f(unsafe { &mut *ptr });
    CONTROLLER.with(|c| c.set(ptr));
    true
}

unsafe extern "system" fn session_proc(hwnd: HWND, message: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match SessionMsg::classify(message, wparam.0) {
        Some(SessionMsg::QueryEnd) => {
            if !with_controller(|ctl| ctl.on_query_end_session(hwnd)) {
                lock(&PENDING).push(SessionMsg::QueryEnd);
            }
            LRESULT(QUERY_ANSWER)
        }
        Some(SessionMsg::End { ending }) => {
            if !with_controller(|ctl| ctl.on_end_session(ending)) && ending {
                log!("record: the session is ending outside the pump; the engine is left to Windows");
            }
            LRESULT(0)
        }
        Some(msg @ (SessionMsg::Lock | SessionMsg::Unlock)) => {
            lock(&PENDING).push(msg);
            LRESULT(0)
        }
        None => unsafe { DefWindowProcW(hwnd, message, wparam, lparam) },
    }
}

impl Controller {
    /// Pump step: the lock notices (and any unanswered session-end query) since the last pass.
    pub(crate) fn session_tick(&mut self, now: Instant) {
        let recording = self.activity.is_recording();
        self.lock.recording_seen(recording);
        let pending = std::mem::take(&mut *lock(&PENDING));
        for msg in pending {
            let actions = match msg {
                SessionMsg::Lock => self.lock.on_lock(now, recording),
                SessionMsg::Unlock => self.lock.on_unlock(now, recording),
                SessionMsg::QueryEnd => {
                    if recording {
                        log!("record: the session is ending -- stopping the long recording (late)");
                        self.send_session_stop();
                    }
                    continue;
                }
                SessionMsg::End { .. } => continue,
            };
            log!("record: session {msg:?} (long recording running: {recording})");
            for action in actions {
                self.apply_lock_action(action);
            }
        }
    }

    fn apply_lock_action(&mut self, action: LockAction) {
        match action {
            LockAction::TellEngine(locked) => {
                // Not a user action: an engine that does not take it only loses the count.
                let _ = self.supervisor.send_engine_line(&engine_line(locked));
            }
            LockAction::Notice(span) => {
                let detail = notice_detail(span);
                log!("record: unlocked -- the long recording kept going while locked ({detail})");
                self.hub.event("session", &app_event_json(span));
                self.shortcut_notice(NOTICE_TITLE, &detail);
            }
        }
    }

    /// A fresh engine learns that the session is locked (it counts from now).
    pub(crate) fn reapply_lock(&self) {
        if self.lock.is_locked() {
            let _ = self.supervisor.send_engine_line(&engine_line(true));
        }
    }

    /// `WM_QUERYENDSESSION`, on the pump thread inside the message drain.
    pub(crate) fn on_query_end_session(&mut self, hwnd: HWND) {
        let recording = self.activity.is_recording();
        log!("record: the session is ending (long recording running: {recording})");
        for action in on_query_end(recording) {
            match action {
                EndAction::Block(_) => {
                    let reason: Vec<u16> = BLOCK_REASON.encode_utf16().chain(Some(0)).collect();
                    let _ = unsafe { ShutdownBlockReasonCreate(hwnd, PCWSTR(reason.as_ptr())) };
                }
                EndAction::StopRecording => self.send_session_stop(),
                EndAction::WaitStopped(wait) => self.wait_recording_stopped(wait),
                EndAction::Unblock => {
                    let _ = unsafe { ShutdownBlockReasonDestroy(hwnd) };
                }
                EndAction::Shutdown(_) => {}
            }
        }
    }

    /// `WM_ENDSESSION`: the clean shutdown runs before Windows may end the process.
    pub(crate) fn on_end_session(&mut self, ending: bool) {
        for action in on_end(ending) {
            if let EndAction::Shutdown(grace) = action {
                log!("record: the session is ending -- shutting down");
                self.quit.get_or_insert(QuitReason::SessionEnd);
                self.shutdown_now(grace);
            }
        }
    }

    fn send_session_stop(&mut self) {
        if !crate::record_dispatch::automatic_effects(self.supervisor.send_engine_line(r#"{"cmd":"record_stop"}"#)).is_empty() {
            log!("record: session end -- record_stop not sent ({})", crate::record_dispatch::NOT_RESPONDING);
        }
    }

    /// Take engine events until the long recording reports stopping, or `wait` runs out.
    fn wait_recording_stopped(&mut self, wait: Duration) {
        let started = Instant::now();
        loop {
            let now = Instant::now();
            self.drain_engine_events(now);
            if !self.activity.is_recording() {
                return log!("record: session end -- long recording saved in {:?}", now - started);
            }
            if now - started >= wait {
                return log!("record: session end -- no recording_stopped in {wait:?}; the .part is recovered on the next start");
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }
}
