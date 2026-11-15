//! When the source-name scan runs (`source::captured_window`: an `EnumWindows` z-order
//! walk, an unnamed monitor capture only). It used to run every second; now a window event
//! that can change the name marks it due, and the 1 s periodic tick runs it then (the rename
//! lands as fast as before). The events: a foreground change, a minimize / restore, the end
//! of a move or resize (a window moved to or off the captured monitor), and the source
//! window renaming itself (EVENT_OBJECT_NAMECHANGE, hooked for the source's process only and
//! acted on only for that window itself). Out-of-context WinEvent hooks only (no code in any
//! other process, anti-cheat safe), installed on the event-loop thread, which pumps the
//! messages they arrive as. No EVENT_OBJECT_LOCATIONCHANGE (it fires on every cursor move).
//!
//! A file named after the source (a saved clip, a long recording, a screenshot) never waits
//! for the tick: [`fresh_name`] scans at that moment. A slow backstop ([`BACKSTOP`]) still
//! scans for what no cheap event reports: a window raised without taking the foreground.

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering::Relaxed};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use windows::Win32::Foundation::HWND;
use windows::Win32::UI::Accessibility::{SetWinEventHook, UnhookWinEvent, HWINEVENTHOOK};
use windows::Win32::UI::WindowsAndMessaging::{
    CHILDID_SELF, EVENT_OBJECT_NAMECHANGE, EVENT_SYSTEM_FOREGROUND, EVENT_SYSTEM_MINIMIZEEND,
    EVENT_SYSTEM_MINIMIZESTART, EVENT_SYSTEM_MOVESIZEEND, OBJID_WINDOW, WINEVENT_OUTOFCONTEXT,
    WINEVENT_SKIPOWNPROCESS,
};

use crate::source;

/// timer-ok: a window raised above the source without taking the foreground has no cheap
/// event (only EVENT_OBJECT_REORDER / LOCATIONCHANGE, both fired constantly).
pub(crate) const BACKSTOP: Duration = Duration::from_secs(5);

/// The event ranges hooked for every process (first, last).
const EVENTS: [(u32, u32); 3] = [
    (EVENT_SYSTEM_FOREGROUND, EVENT_SYSTEM_FOREGROUND),
    (EVENT_SYSTEM_MOVESIZEEND, EVENT_SYSTEM_MOVESIZEEND),
    (EVENT_SYSTEM_MINIMIZESTART, EVENT_SYSTEM_MINIMIZEEND),
];

/// A hooked event arrived since the last scan (set by the hooks, taken by [`SourceWatch::due`]).
static CHANGED: AtomicBool = AtomicBool::new(false);
/// The source window whose renames count (0: none yet).
static SOURCE_HWND: AtomicUsize = AtomicUsize::new(0);

/// The hooks (installed on the first [`SourceWatch::due`]) and the last scan's time.
#[derive(Default)]
pub(crate) struct SourceWatch {
    tried: bool,
    hooks: Vec<HWINEVENTHOOK>,
    /// The rename hook and the process it is scoped to.
    name_hook: Option<(u32, HWINEVENTHOOK)>,
    last_scan: Option<Instant>,
}

impl SourceWatch {
    /// Whether to scan now (event-loop thread: the hooks are installed here, once).
    pub(crate) fn due(&mut self, now: Instant) -> bool {
        if !self.tried {
            self.tried = true;
            self.hooks = EVENTS.iter().filter_map(|&(first, last)| hook(first, last, 0, on_event)).collect();
            if self.hooks.len() < EVENTS.len() {
                eprintln!("[recall] source watch: {} of {} window hooks installed; scanning every second", self.hooks.len(), EVENTS.len());
            }
        }
        let hooked = self.hooks.len() == EVENTS.len();
        let changed = CHANGED.swap(false, Relaxed);
        let due = scan_due(hooked, changed, self.last_scan, now);
        if due {
            self.last_scan = Some(now);
        }
        due
    }

    /// The scan found `window` (event-loop thread): follow its renames. The rename hook is
    /// moved only when the source's process changes.
    pub(crate) fn follow(&mut self, window: &source::Found) {
        SOURCE_HWND.store(window.hwnd, Relaxed);
        if self.name_hook.is_some_and(|(pid, _)| pid == window.pid) {
            return;
        }
        if let Some((_, old)) = self.name_hook.take() {
            unhook(old);
        }
        self.name_hook = hook(EVENT_OBJECT_NAMECHANGE, EVENT_OBJECT_NAMECHANGE, window.pid, on_rename).map(|h| (window.pid, h));
    }
}

impl Drop for SourceWatch {
    fn drop(&mut self) {
        let named = self.name_hook.take().map(|(_, h)| h);
        self.hooks.drain(..).chain(named).for_each(unhook);
    }
}

/// Scan when the hooks are missing (the old 1 s scan), an event came, there was no scan
/// yet, or the backstop ran out.
pub(crate) fn scan_due(hooked: bool, changed: bool, last: Option<Instant>, now: Instant) -> bool {
    !hooked || changed || last.is_none_or(|t| now.duration_since(t) >= BACKSTOP)
}

/// No controller-given name: the source follows the topmost window on the monitor.
pub(crate) fn unnamed_capture() -> bool {
    std::env::var_os("RECALL_CAPTURE_HWND").is_none() && std::env::var_os("RECALL_CAPTURE_NAME").is_none()
}

/// The source name for a file named now (any thread): scanned at this moment for an unnamed
/// capture, so a save never takes a name up to [`BACKSTOP`] old.
pub(crate) fn fresh_name(shared: &Mutex<String>) -> String {
    fresh_name_with(shared, unnamed_capture(), source::captured_source)
}

/// [`fresh_name`] over a given scan. A new name is stored in `shared` and the loop's own scan
/// is marked due (it renames the window and the status).
pub(crate) fn fresh_name_with(shared: &Mutex<String>, unnamed: bool, scan: impl FnOnce() -> Option<String>) -> String {
    let scanned = unnamed.then(scan).flatten(); // before the lock: the scan takes a few ms
    let Ok(mut name) = shared.lock() else { return String::new() };
    if let Some(now) = scanned {
        if *name != now {
            *name = now;
            CHANGED.store(true, Relaxed);
        }
    }
    name.clone()
}

type Callback = unsafe extern "system" fn(HWINEVENTHOOK, u32, HWND, i32, i32, u32, u32);

/// An out-of-context hook for `first..=last` (`pid` 0: every process) on this thread.
fn hook(first: u32, last: u32, pid: u32, callback: Callback) -> Option<HWINEVENTHOOK> {
    // SAFETY: `callback` is a 'static extern "system" fn; hmod 0 + OUTOFCONTEXT: delivered to
    // this thread's message loop, nothing loaded into another process.
    let h = unsafe { SetWinEventHook(first, last, None, Some(callback), pid, 0, WINEVENT_OUTOFCONTEXT | WINEVENT_SKIPOWNPROCESS) };
    (!h.0.is_null()).then_some(h)
}

fn unhook(h: HWINEVENTHOOK) {
    // SAFETY: a handle from `SetWinEventHook` on this thread, unhooked once.
    let _ = unsafe { UnhookWinEvent(h) };
}

unsafe extern "system" fn on_event(_: HWINEVENTHOOK, _: u32, _: HWND, _: i32, _: i32, _: u32, _: u32) {
    CHANGED.store(true, Relaxed);
}

unsafe extern "system" fn on_rename(_: HWINEVENTHOOK, _: u32, hwnd: HWND, object: i32, child: i32, _: u32, _: u32) {
    if is_source_rename(hwnd.0 as usize, object, child, SOURCE_HWND.load(Relaxed)) {
        CHANGED.store(true, Relaxed);
    }
}

/// A rename counts only for the source window itself (not one of its controls).
fn is_source_rename(hwnd: usize, object: i32, child: i32, source: usize) -> bool {
    source != 0 && hwnd == source && object == OBJID_WINDOW.0 && child == CHILDID_SELF as i32
}

#[cfg(test)]
#[path = "source_watch_tests.rs"]
mod tests;
