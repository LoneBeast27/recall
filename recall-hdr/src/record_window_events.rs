//! WinEvent hooks that wake the pump when the recorded window changes, in place of the
//! 50 ms pump tick that used to re-read it (`record_pump`).
//!
//! Scoped to the process that owns the target's (and the pinned) window:
//! - `EVENT_OBJECT_DESTROY` / `HIDE`: the window closed, or hid (the 5 s hidden rule,
//!   `record_visibility`, then runs to its own deadline). Not `SHOW`: a window shown again
//!   is read on the rule's own deadline, and cursor show/hide (`OBJID_CURSOR`, raised in the
//!   game's process) would wake the pump for nothing;
//! - `EVENT_OBJECT_CLOAKED` / `UNCLOAKED`: a virtual-desktop switch;
//! - `EVENT_SYSTEM_MINIMIZESTART` / `MINIMIZEEND` and `EVENT_SYSTEM_MOVESIZEEND`: the
//!   fullscreen-only read (`record_detect`) and the encoder watch's sight (`record_screen`).
//!
//! And one hook for every process: `EVENT_SYSTEM_DESKTOPSWITCH` (the lock screen, UAC and
//! Ctrl+Alt+Del switch the input desktop), which also drops `record_screen`'s cached
//! input-desktop answer.
//!
//! The hooks only wake the pump: every pass runs `tick`, which re-reads the window as it
//! always did. All are `WINEVENT_OUTOFCONTEXT` (delivered to this thread as messages,
//! nothing loads into the game) and none is `EVENT_OBJECT_LOCATIONCHANGE` (184 cursor
//! events a second even when scoped, `record_detect`). A hook's delivery wakes the pump
//! whatever the event's object, so the callback cannot filter the wake itself; it counts
//! window events (`OBJID_WINDOW`, `CHILDID_SELF`) and the rest (a cursor or caret hiding)
//! apart, and both counts per hooked process are logged when the hooks move on, so a game
//! whose other objects are chatty shows in the log.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

use windows::Win32::Foundation::HWND;
use windows::Win32::UI::Accessibility::{SetWinEventHook, UnhookWinEvent, HWINEVENTHOOK};
use windows::Win32::UI::WindowsAndMessaging::{
    GetWindowThreadProcessId, EVENT_OBJECT_CLOAKED, EVENT_OBJECT_DESTROY, EVENT_OBJECT_HIDE, EVENT_OBJECT_UNCLOAKED,
    EVENT_SYSTEM_DESKTOPSWITCH, EVENT_SYSTEM_MINIMIZEEND, EVENT_SYSTEM_MINIMIZESTART, EVENT_SYSTEM_MOVESIZEEND,
    WINEVENT_OUTOFCONTEXT,
};

use crate::log;

/// The event ranges hooked per process (`EVENT_OBJECT_SHOW`, between DESTROY and HIDE, is not).
pub(crate) const SCOPED: [(u32, u32); 5] = [
    (EVENT_OBJECT_DESTROY, EVENT_OBJECT_DESTROY),
    (EVENT_OBJECT_HIDE, EVENT_OBJECT_HIDE),
    (EVENT_OBJECT_CLOAKED, EVENT_OBJECT_UNCLOAKED),
    (EVENT_SYSTEM_MINIMIZESTART, EVENT_SYSTEM_MINIMIZEEND),
    (EVENT_SYSTEM_MOVESIZEEND, EVENT_SYSTEM_MOVESIZEEND),
];

/// Window events delivered to the scoped hooks since the process was hooked.
static SCOPED_EVENTS: AtomicU64 = AtomicU64::new(0);
/// Events for other objects of that process (cursor, caret, child objects).
static OTHER_EVENTS: AtomicU64 = AtomicU64::new(0);
/// `OBJID_WINDOW` and `CHILDID_SELF`.
const OBJID_WINDOW: i32 = 0;
const CHILDID_SELF: i32 = 0;

/// The event is about a window itself (not its cursor, caret or a child object).
pub(crate) fn is_window_event(id_object: i32, id_child: i32) -> bool {
    id_object == OBJID_WINDOW && id_child == CHILDID_SELF
}

/// The process that owns `hwnd` (0 or a gone window: none).
pub(crate) fn window_pid(hwnd: isize) -> Option<u32> {
    let mut pid = 0u32;
    // SAFETY: tolerates a stale handle (pid stays 0).
    unsafe { GetWindowThreadProcessId(HWND(hwnd as *mut _), Some(&mut pid)) };
    (pid != 0 && pid != std::process::id()).then_some(pid)
}

/// The installed hooks. Lives on the pump thread (hooks are unhooked where they were made).
pub(crate) struct WindowEvents {
    pids: Vec<u32>,
    scoped: Vec<HWINEVENTHOOK>,
    since: Instant,
    desktop: Option<HWINEVENTHOOK>,
}

impl WindowEvents {
    /// The desktop-switch hook (any process); the scoped ones come with [`Self::follow`].
    pub(crate) fn new() -> Self {
        let desktop = hook(EVENT_SYSTEM_DESKTOPSWITCH, EVENT_SYSTEM_DESKTOPSWITCH, 0, on_desktop_switch);
        if desktop.is_none() {
            log!("record: desktop switch hook unavailable; the input desktop is read at most once a second");
        }
        WindowEvents { pids: Vec::new(), scoped: Vec::new(), since: Instant::now(), desktop }
    }

    /// Hook the processes in `pids` (re-hooked only when the set changes).
    pub(crate) fn follow(&mut self, pids: &[u32]) {
        let mut want = pids.to_vec();
        want.sort_unstable();
        want.dedup();
        if want == self.pids {
            return;
        }
        self.unhook_scoped();
        for &pid in &want {
            for (min, max) in SCOPED {
                match hook(min, max, pid, on_window_event) {
                    Some(h) => self.scoped.push(h),
                    None => log!("record: window event hook {min:#x}-{max:#x} for pid {pid} failed; its checks ride the 1 s pass"),
                }
            }
        }
        self.pids = want;
    }

    fn unhook_scoped(&mut self) {
        let events = SCOPED_EVENTS.swap(0, Ordering::Relaxed);
        let other = OTHER_EVENTS.swap(0, Ordering::Relaxed);
        if !self.pids.is_empty() {
            log!("record: window events for pid {:?}: {events} (+{other} for other objects) in {} s", self.pids, self.since.elapsed().as_secs());
        }
        for h in self.scoped.drain(..) {
            // SAFETY: a hook made on this thread, unhooked once.
            let _ = unsafe { UnhookWinEvent(h) };
        }
        self.since = Instant::now();
    }
}

impl Drop for WindowEvents {
    fn drop(&mut self) {
        self.unhook_scoped();
        if let Some(h) = self.desktop.take() {
            // SAFETY: as above.
            let _ = unsafe { UnhookWinEvent(h) };
        }
    }
}

type Callback = unsafe extern "system" fn(HWINEVENTHOOK, u32, HWND, i32, i32, u32, u32);

/// An out-of-context hook for `min..=max` on `pid` (0 = every process).
fn hook(min: u32, max: u32, pid: u32, callback: Callback) -> Option<HWINEVENTHOOK> {
    // SAFETY: a 'static callback; no module: out of context, nothing is injected.
    let h = unsafe { SetWinEventHook(min, max, None, Some(callback), pid, 0, WINEVENT_OUTOFCONTEXT) };
    (!h.0.is_null()).then_some(h)
}

/// The message that delivered this already woke the pump; only counted.
unsafe extern "system" fn on_window_event(_: HWINEVENTHOOK, _: u32, _: HWND, id_object: i32, id_child: i32, _: u32, _: u32) {
    let count = if is_window_event(id_object, id_child) { &SCOPED_EVENTS } else { &OTHER_EVENTS };
    count.fetch_add(1, Ordering::Relaxed);
}

unsafe extern "system" fn on_desktop_switch(_: HWINEVENTHOOK, _: u32, _: HWND, _: i32, _: i32, _: u32, _: u32) {
    crate::record_screen::desktop_switched();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_scoped_ranges_are_the_window_events_and_never_locationchange() {
        const LOCATIONCHANGE: u32 = 0x800B;
        for (min, max) in SCOPED {
            assert!(min <= max);
            assert!(!(min..=max).contains(&LOCATIONCHANGE), "{min:#x}-{max:#x}");
        }
        let covered = |e: u32| SCOPED.iter().any(|(a, b)| (*a..=*b).contains(&e));
        for e in [0x8001, 0x8003, 0x8017, 0x8018, 0x0016, 0x0017, 0x000B] {
            assert!(covered(e), "{e:#x}");
        }
        assert!(!covered(0x8002), "no EVENT_OBJECT_SHOW (cursor show would wake the pump)");
        assert_eq!(SCOPED.iter().map(|(a, b)| b - a + 1).sum::<u32>(), 7, "nothing else");
        assert!(is_window_event(0, 0));
        assert!(!is_window_event(-9, 0), "OBJID_CURSOR");
        assert!(!is_window_event(-8, 0), "OBJID_CARET");
        assert!(!is_window_event(0, 3), "a child element");
    }

    #[test]
    fn the_hooks_are_out_of_context_only() {
        let src = include_str!("record_window_events.rs");
        let body = &src[src.find("fn hook(").expect("hook")..];
        let body = &body[..body.find("\n}").expect("end")];
        assert!(body.contains("WINEVENT_OUTOFCONTEXT") && !body.contains("INCONTEXT"), "{body}");
    }

    #[test]
    fn no_window_names_no_process() {
        assert_eq!(window_pid(0), None);
    }
}
