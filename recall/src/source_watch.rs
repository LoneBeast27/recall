//! When the source-name scan runs (`source::captured_source`: an `EnumWindows` z-order
//! walk, an unnamed monitor capture only). It used to run every second; now a window event
//! that can change the topmost window on the captured monitor marks it due, and the 1 s
//! periodic tick runs it then (the rename lands as fast as before). The events: a foreground
//! change, a minimize / restore, and the end of a move or resize (a window moved to or off
//! the captured monitor). Out-of-context WinEvent hooks only (no code in any other process,
//! anti-cheat safe), installed on the event-loop thread, which pumps the messages they
//! arrive as. No EVENT_OBJECT_LOCATIONCHANGE (it fires on every cursor move).
//!
//! A slow backstop ([`BACKSTOP`]) still scans for what no cheap event reports: a window
//! raised without taking the foreground, or a game renaming its own window.

use std::sync::atomic::{AtomicBool, Ordering::Relaxed};
use std::time::{Duration, Instant};

use windows::Win32::Foundation::HWND;
use windows::Win32::UI::Accessibility::{SetWinEventHook, UnhookWinEvent, HWINEVENTHOOK};
use windows::Win32::UI::WindowsAndMessaging::{
    EVENT_SYSTEM_FOREGROUND, EVENT_SYSTEM_MINIMIZEEND, EVENT_SYSTEM_MINIMIZESTART, EVENT_SYSTEM_MOVESIZEEND,
    WINEVENT_OUTOFCONTEXT, WINEVENT_SKIPOWNPROCESS,
};

/// timer-ok: z-order changes without a foreground change and window title changes have no
/// cheap event (EVENT_OBJECT_NAMECHANGE fires for every control's text in every process).
pub(crate) const BACKSTOP: Duration = Duration::from_secs(5);

/// The event ranges hooked (first, last).
const EVENTS: [(u32, u32); 3] = [
    (EVENT_SYSTEM_FOREGROUND, EVENT_SYSTEM_FOREGROUND),
    (EVENT_SYSTEM_MOVESIZEEND, EVENT_SYSTEM_MOVESIZEEND),
    (EVENT_SYSTEM_MINIMIZESTART, EVENT_SYSTEM_MINIMIZEEND),
];

/// A hooked event arrived since the last scan (set by the hook, taken by [`SourceWatch::due`]).
static CHANGED: AtomicBool = AtomicBool::new(false);

/// The hooks (installed on the first [`SourceWatch::due`]) and the last scan's time.
#[derive(Default)]
pub(crate) struct SourceWatch {
    tried: bool,
    hooks: Vec<HWINEVENTHOOK>,
    last_scan: Option<Instant>,
}

impl SourceWatch {
    /// Whether to scan now (event-loop thread: the hooks are installed here, once).
    pub(crate) fn due(&mut self, now: Instant) -> bool {
        if !self.tried {
            self.tried = true;
            self.hooks = install();
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
}

impl Drop for SourceWatch {
    fn drop(&mut self) {
        for hook in self.hooks.drain(..) {
            // SAFETY: a handle from `SetWinEventHook` on this thread, unhooked once.
            let _ = unsafe { UnhookWinEvent(hook) };
        }
    }
}

/// Scan when the hooks are missing (the old 1 s scan), an event came, there was no scan
/// yet, or the backstop ran out.
pub(crate) fn scan_due(hooked: bool, changed: bool, last: Option<Instant>, now: Instant) -> bool {
    !hooked || changed || last.is_none_or(|t| now.duration_since(t) >= BACKSTOP)
}

fn install() -> Vec<HWINEVENTHOOK> {
    EVENTS
        .iter()
        .filter_map(|&(first, last)| {
            // SAFETY: `on_event` is a 'static extern "system" fn; hmod/pid/tid 0 = every
            // process, delivered out of context to this thread's message loop.
            let hook = unsafe {
                SetWinEventHook(first, last, None, Some(on_event), 0, 0, WINEVENT_OUTOFCONTEXT | WINEVENT_SKIPOWNPROCESS)
            };
            (!hook.0.is_null()).then_some(hook)
        })
        .collect()
}

unsafe extern "system" fn on_event(_: HWINEVENTHOOK, _: u32, _: HWND, _: i32, _: i32, _: u32, _: u32) {
    CHANGED.store(true, Relaxed);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scans_on_an_event_or_the_backstop_only() {
        let t0 = Instant::now();
        let soon = t0 + Duration::from_secs(1);
        assert!(scan_due(true, false, None, t0), "the first scan runs at once");
        assert!(!scan_due(true, false, Some(t0), soon), "no event, inside the backstop: no scan");
        assert!(scan_due(true, true, Some(t0), soon), "a foreground / move / minimize event scans");
        assert!(scan_due(true, false, Some(t0), t0 + BACKSTOP), "the backstop scans");
    }

    /// The real hooks install (out of context: nothing is loaded into another process), and
    /// with them the second tick of a quiet desktop skips the scan.
    #[test]
    fn the_hooks_install_and_quiet_ticks_skip_the_scan() {
        let mut watch = SourceWatch::default();
        let t0 = Instant::now();
        assert!(watch.due(t0), "the first scan runs");
        assert_eq!(watch.hooks.len(), EVENTS.len(), "every hook installed");
        // This test thread pumps no messages, so no hooked event can arrive here.
        assert!(!watch.due(t0 + Duration::from_secs(1)));
        assert!(watch.due(t0 + BACKSTOP));
    }

    #[test]
    fn without_hooks_it_scans_every_tick() {
        let t0 = Instant::now();
        assert!(scan_due(false, false, Some(t0), t0 + Duration::from_secs(1)));
    }
}
