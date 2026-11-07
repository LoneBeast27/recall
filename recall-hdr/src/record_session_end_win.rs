//! Thin Win32 glue for `record_session_end`: a hidden top-level window on the pump thread
//! (session-end messages go to top-level windows only, never to message-only ones), the
//! shutdown block reason, and the pump step that turns its notices into a quit.
//!
//! The window procedure runs inside the pump's message drain. It only answers
//! `WM_QUERYENDSESSION` from the block flag the pump last set and queues what it saw;
//! the pump acts on the queue on its next pass.

use std::sync::atomic::{AtomicBool, AtomicIsize, Ordering};
use std::sync::{Mutex, MutexGuard};
use std::time::Instant;

use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, RegisterClassW, WM_ENDSESSION, WM_QUERYENDSESSION, WNDCLASSW,
    WS_EX_TOOLWINDOW, WS_POPUP,
};

use crate::log;
use crate::record_app::Controller;
use crate::record_quit::QuitReason;
use crate::record_session_end::{query_answer, SessionAction, SessionNotice, BLOCK_REASON};

#[link(name = "user32")]
extern "system" {
    fn ShutdownBlockReasonCreate(hwnd: HWND, reason: PCWSTR) -> i32;
    fn ShutdownBlockReasonDestroy(hwnd: HWND) -> i32;
}

/// The hidden window (0 = none).
static WINDOW: AtomicIsize = AtomicIsize::new(0);
/// A block reason is held now (set by the pump, read by the window procedure).
static BLOCKING: AtomicBool = AtomicBool::new(false);
/// Notices not yet taken by the pump.
static NOTICES: Mutex<Vec<SessionNotice>> = Mutex::new(Vec::new());

fn notices() -> MutexGuard<'static, Vec<SessionNotice>> {
    NOTICES.lock().unwrap_or_else(|p| p.into_inner())
}

fn window() -> Option<HWND> {
    let raw = WINDOW.load(Ordering::Acquire);
    (raw != 0).then_some(HWND(raw as *mut _))
}

unsafe extern "system" fn session_proc(hwnd: HWND, message: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match message {
        WM_QUERYENDSESSION => {
            let held = BLOCKING.load(Ordering::Acquire);
            notices().push(SessionNotice::Query { held });
            LRESULT(query_answer(held) as isize)
        }
        WM_ENDSESSION => {
            notices().push(SessionNotice::End { ending: wparam.0 != 0 });
            LRESULT(0)
        }
        _ => unsafe { DefWindowProcW(hwnd, message, wparam, lparam) },
    }
}

/// Create the hidden window at controller start-up, on the pump thread.
pub(crate) fn create() {
    let instance = match unsafe { GetModuleHandleW(None) } {
        Ok(h) => h,
        Err(e) => {
            log!("record: session-end window unavailable ({e}); a sign-out mid-save is not held");
            return;
        }
    };
    let class_name = w!("RecallSessionEnd");
    let class = WNDCLASSW { lpfnWndProc: Some(session_proc), hInstance: instance.into(), lpszClassName: class_name, ..Default::default() };
    if unsafe { RegisterClassW(&class) } == 0 {
        log!("record: session-end window class not registered; a sign-out mid-save is not held");
        return;
    }
    // Never shown: WS_POPUP without WS_VISIBLE, a tool window (no taskbar button).
    let made = unsafe {
        CreateWindowExW(WS_EX_TOOLWINDOW, class_name, w!("Recall"), WS_POPUP, 0, 0, 0, 0, None, None, Some(instance.into()), None)
    };
    match made {
        Ok(hwnd) => WINDOW.store(hwnd.0 as isize, Ordering::Release),
        Err(e) => log!("record: session-end window not created ({e}); a sign-out mid-save is not held"),
    }
}

/// Hold (`on`) or release the shutdown block reason.
fn set_blocking(on: bool) {
    BLOCKING.store(on, Ordering::Release);
    let Some(hwnd) = window() else { return };
    let ok = if on {
        let reason: Vec<u16> = BLOCK_REASON.encode_utf16().chain(Some(0)).collect();
        unsafe { ShutdownBlockReasonCreate(hwnd, PCWSTR(reason.as_ptr())) }
    } else {
        unsafe { ShutdownBlockReasonDestroy(hwnd) }
    };
    if ok == 0 {
        log!("record: shutdown block reason {} failed", if on { "create" } else { "destroy" });
    }
}

/// Release the reason and the window at controller shutdown.
pub(crate) fn destroy() {
    if BLOCKING.load(Ordering::Acquire) {
        set_blocking(false);
    }
    if let Some(hwnd) = window() {
        let _ = unsafe { DestroyWindow(hwnd) };
        WINDOW.store(0, Ordering::Release);
    }
}

impl Controller {
    /// Pump step: hold the block reason while a save is written, and act on the
    /// session-end notices that arrived since the last tick.
    pub(crate) fn session_end_tick(&mut self, now: Instant) {
        let saving = self.activity.saves_writing(now);
        if let Some(on) = self.block_reason.update(saving) {
            set_blocking(on);
        }
        let seen = std::mem::take(&mut *notices());
        for notice in seen {
            match self.session_end.on_notice(notice) {
                SessionAction::Quit => {
                    log!("record: the session is ending during a save; finishing it, then quitting");
                    self.quit.get_or_insert(QuitReason::SessionEnd);
                }
                SessionAction::Resume if self.quit == Some(QuitReason::SessionEnd) => {
                    log!("record: the session end was cancelled; Recall keeps running");
                    self.quit = None;
                    self.quit_wait.reset();
                }
                SessionAction::Resume | SessionAction::Nothing => {}
            }
        }
    }
}
