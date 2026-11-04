//! Scoped `EVENT_OBJECT_LOCATIONCHANGE` hook on ONE window (the record mode's
//! foreground, while it is not the committed target): a game that turns
//! fullscreen after it took focus is seen at once instead of at the next 1 s
//! probe. No `EVENT_SYSTEM_FOREGROUND` fires for that change.
//!
//! - The hook is scoped to the window's process and thread
//!   (`SetWinEventHook(idProcess, idThread)`), out of context, so the OS only
//!   queues events raised by that thread. Everything except the followed
//!   window's own `OBJID_WINDOW` / `CHILDID_SELF` move is dropped after two
//!   integer compares; a kept move costs one [`fullscreen_on_own_monitor`]
//!   (monitor info + window rect + style), and an event is sent only when the
//!   fullscreen verdict flips.
//! - Same threading contract as the foreground hook: installed, delivered and
//!   unhooked on the pump thread ([`super::Watcher::follow`]).
//! - [`stats`] counts what the callback saw (for the cost check in
//!   `watcher_tests.rs`); relaxed atomic adds, no lock.

use std::panic::catch_unwind;
use std::sync::atomic::{AtomicU64, Ordering::Relaxed};
use std::sync::mpsc::Sender;
use std::sync::Mutex;
use std::time::Instant;

use windows::Win32::Foundation::HWND;
use windows::Win32::UI::Accessibility::{SetWinEventHook, UnhookWinEvent, HWINEVENTHOOK};
use windows::Win32::UI::WindowsAndMessaging::{
    GetWindowThreadProcessId, EVENT_OBJECT_LOCATIONCHANGE, WINEVENT_OUTOFCONTEXT,
};

use super::{build_event, fullscreen_on_own_monitor, WatchEvent};

const OBJID_WINDOW: i32 = 0;
const OBJID_CARET: i32 = -8;
const OBJID_CURSOR: i32 = -9;
const CHILDID_SELF: i32 = 0;

/// The installed scoped hook. Not `Send`: unhook on the pump thread.
#[derive(Debug)]
pub(super) struct FollowHook {
    hook: HWINEVENTHOOK,
}

/// What the callback needs: the followed window, the last fullscreen verdict
/// sent (or measured at install), and where to send a flip.
struct FollowState {
    hwnd: isize,
    fullscreen: bool,
    tx: Sender<WatchEvent>,
    target_monitor: isize,
}

static FOLLOW: Mutex<Option<FollowState>> = Mutex::new(None);

/// Callback counters since process start.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct FollowStats {
    /// Every callback.
    pub events: u64,
    /// `OBJID_CURSOR` moves (dropped).
    pub cursor: u64,
    /// `OBJID_CARET` moves (dropped).
    pub caret: u64,
    /// Other objects or children (dropped).
    pub other_object: u64,
    /// `OBJID_WINDOW` moves of another window of the same thread (dropped).
    pub other_window: u64,
    /// Moves of the followed window: fullscreen verdict computed.
    pub checked: u64,
    /// Verdict flips sent to the pump.
    pub sent: u64,
    /// Time spent on checked moves (ns).
    pub checked_ns: u64,
}

static EVENTS: AtomicU64 = AtomicU64::new(0);
static CURSOR: AtomicU64 = AtomicU64::new(0);
static CARET: AtomicU64 = AtomicU64::new(0);
static OTHER_OBJECT: AtomicU64 = AtomicU64::new(0);
static OTHER_WINDOW: AtomicU64 = AtomicU64::new(0);
static CHECKED: AtomicU64 = AtomicU64::new(0);
static SENT: AtomicU64 = AtomicU64::new(0);
static CHECKED_NS: AtomicU64 = AtomicU64::new(0);

pub fn stats() -> FollowStats {
    FollowStats {
        events: EVENTS.load(Relaxed),
        cursor: CURSOR.load(Relaxed),
        caret: CARET.load(Relaxed),
        other_object: OTHER_OBJECT.load(Relaxed),
        other_window: OTHER_WINDOW.load(Relaxed),
        checked: CHECKED.load(Relaxed),
        sent: SENT.load(Relaxed),
        checked_ns: CHECKED_NS.load(Relaxed),
    }
}

fn slot() -> std::sync::MutexGuard<'static, Option<FollowState>> {
    FOLLOW.lock().unwrap_or_else(|p| p.into_inner())
}

/// Hook `hwnd`'s thread for location changes. `None` when the window is gone
/// or the hook cannot be installed.
pub(super) fn start(hwnd: isize, tx: Sender<WatchEvent>, target_monitor: isize) -> Option<FollowHook> {
    let mut pid = 0u32;
    // SAFETY: tolerates a stale handle (returns thread 0).
    let tid = unsafe { GetWindowThreadProcessId(HWND(hwnd as *mut _), Some(&mut pid)) };
    if tid == 0 || pid == 0 {
        return None;
    }
    let fullscreen = fullscreen_on_own_monitor(HWND(hwnd as *mut _)).0;
    *slot() = Some(FollowState { hwnd, fullscreen, tx, target_monitor });
    // SAFETY: `location_hook` is a 'static `extern "system"` routine; out of
    // context, scoped to one process and thread.
    let hook = unsafe {
        SetWinEventHook(
            EVENT_OBJECT_LOCATIONCHANGE,
            EVENT_OBJECT_LOCATIONCHANGE,
            None,
            Some(location_hook),
            pid,
            tid,
            WINEVENT_OUTOFCONTEXT,
        )
    };
    if hook.0.is_null() {
        *slot() = None;
        return None;
    }
    Some(FollowHook { hook })
}

/// Unhook and forget the followed window.
pub(super) fn stop(f: FollowHook) {
    // SAFETY: a hook this thread installed and has not unhooked.
    let _ = unsafe { UnhookWinEvent(f.hook) };
    *slot() = None;
}

/// Whether a scoped hook is installed (tests).
#[cfg(test)]
pub(super) fn installed() -> bool {
    slot().is_some()
}

unsafe extern "system" fn location_hook(
    _hook: HWINEVENTHOOK,
    _event: u32,
    hwnd: HWND,
    id_object: i32,
    id_child: i32,
    _id_event_thread: u32,
    _ms_event_time: u32,
) {
    let _ = catch_unwind(|| on_location(hwnd.0 as isize, id_object, id_child));
}

fn on_location(hwnd: isize, id_object: i32, id_child: i32) {
    EVENTS.fetch_add(1, Relaxed);
    if id_object != OBJID_WINDOW || id_child != CHILDID_SELF {
        let counter = match id_object {
            OBJID_CURSOR => &CURSOR,
            OBJID_CARET => &CARET,
            _ => &OTHER_OBJECT,
        };
        counter.fetch_add(1, Relaxed);
        return;
    }
    if slot().as_ref().map(|s| s.hwnd) != Some(hwnd) {
        OTHER_WINDOW.fetch_add(1, Relaxed);
        return;
    }
    let t = Instant::now();
    let fullscreen = fullscreen_on_own_monitor(HWND(hwnd as *mut _)).0;
    let flip = {
        let mut guard = slot();
        match guard.as_mut() {
            Some(s) if s.hwnd == hwnd && s.fullscreen != fullscreen => {
                s.fullscreen = fullscreen;
                Some((s.tx.clone(), s.target_monitor))
            }
            _ => None,
        }
    };
    if let Some((tx, target_monitor)) = flip {
        if let Some(ev) = build_event(HWND(hwnd as *mut _), target_monitor) {
            let _ = tx.send(ev);
            SENT.fetch_add(1, Relaxed);
        }
    }
    CHECKED.fetch_add(1, Relaxed);
    CHECKED_NS.fetch_add(t.elapsed().as_nanos() as u64, Relaxed);
}
