//! The primary monitor, followed at run time (audit T14). The watcher's
//! managed monitor (`target_monitor`, the `fullscreen_on_target` flag) and the
//! engine's Desktop capture (`Monitor::primary()` at engine start) were read
//! once at start-up, so a hot-plug or a new primary display was not followed.
//! The controller re-reads the primary (two cheap queries) when Windows says the
//! displays changed (`WM_DISPLAYCHANGE` / `WM_SETTINGCHANGE` on the session
//! window, `record_session_win`), and every [`SAFETY_EVERY`] in case a broadcast
//! was missed (every [`CHECK_EVERY`] when that window could not be made); a new
//! reading counts once it held for [`SETTLE`]. A new handle
//! updates the watcher; a different display as primary restarts the Desktop
//! capture (only while the engine records the desktop; a game capture follows
//! its window).

use std::time::{Duration, Instant};

use windows::Win32::Foundation::POINT;
use windows::Win32::Graphics::Gdi::{GetMonitorInfoW, MonitorFromPoint, MONITORINFO, MONITORINFOEXW, MONITOR_DEFAULTTOPRIMARY};

use crate::log;
use crate::record_app::Controller;
use crate::record_engine::EngineTarget;

/// How often the primary monitor is re-read with no display-change messages.
// timer-ok: only when the session window (the messages' receiver) could not be made.
pub const CHECK_EVERY: Duration = Duration::from_secs(5);
/// How often it is re-read anyway while the messages arrive.
// timer-ok: a safety net for a missed broadcast; it rides the pump's 1 s pass.
pub const SAFETY_EVERY: Duration = Duration::from_secs(60);
/// A new reading is re-read after this and reported only if it held, so a
/// DisplayPort link drop (the other display primary for a moment) restarts
/// nothing, and a real change restarts once.
pub const SETTLE: Duration = Duration::from_secs(2);

/// The primary display: its `HMONITOR` (as `isize`) and GDI device name
/// (`\\.\DISPLAY1`). A handle can change on a display reconfigure while the
/// same display stays primary; the device name tells the two apart.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Primary {
    pub handle: isize,
    pub device: String,
}

/// What changed since the last read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrimaryChange {
    pub now: Primary,
    /// Another display is primary now (else only the handle changed).
    pub moved: bool,
}

pub struct PrimaryWatch {
    last: Primary,
    checked: Instant,
    /// A new reading, re-read after [`SETTLE`].
    settling: Option<Primary>,
    /// The re-read period: [`CHECK_EVERY`], or [`SAFETY_EVERY`] while display changes are heard.
    every: Duration,
    /// Windows said the displays changed: read on the next poll.
    poked: bool,
}

impl PrimaryWatch {
    pub fn new(primary: Primary, now: Instant) -> Self {
        PrimaryWatch { last: primary, checked: now, settling: None, every: CHECK_EVERY, poked: false }
    }

    /// Display-change messages arrive (`true`): the timed re-read is only a safety net.
    pub fn hear_changes(&mut self, heard: bool) {
        self.every = if heard { SAFETY_EVERY } else { CHECK_EVERY };
    }

    /// Windows said the displays changed: the next poll reads (a settle still waits [`SETTLE`]:
    /// a link drop sends several messages, and the settle is what tells it from a change).
    pub fn poke(&mut self) {
        self.poked = true;
    }

    /// When a new reading is due to be confirmed (for the pump's wait).
    pub fn deadline(&self) -> Option<Instant> {
        self.settling.as_ref().map(|_| self.checked + SETTLE)
    }

    /// Re-read the primary with `read` when poked or the period has passed (or
    /// [`SETTLE`] after a new reading). A change is reported once it held.
    pub fn poll(&mut self, now: Instant, read: impl FnOnce() -> Primary) -> Option<PrimaryChange> {
        let wait = match (self.settling.is_some(), self.poked) {
            (true, _) => SETTLE,
            (false, true) => Duration::ZERO,
            (false, false) => self.every,
        };
        if now.duration_since(self.checked) < wait {
            return None;
        }
        self.checked = now;
        self.poked = false;
        let primary = read();
        if primary == self.last {
            self.settling = None; // it came back: a link drop, nothing to do
            return None;
        }
        match self.settling.take() {
            Some(seen) if seen == primary => {}
            _ => {
                self.settling = Some(primary);
                return None;
            }
        }
        let moved = primary.device != self.last.device;
        self.last = primary.clone();
        Some(PrimaryChange { now: primary, moved })
    }
}

/// The engine restarts for `change` only when it records the desktop.
pub fn restarts_desktop(change: &PrimaryChange, engine: &EngineTarget) -> bool {
    change.moved && *engine == EngineTarget::Desktop
}

/// The live primary monitor.
pub fn read_primary() -> Primary {
    // SAFETY: plain queries; MONITOR_DEFAULTTOPRIMARY never returns null.
    let mon = unsafe { MonitorFromPoint(POINT { x: 0, y: 0 }, MONITOR_DEFAULTTOPRIMARY) };
    let mut info = MONITORINFOEXW::default();
    info.monitorInfo.cbSize = std::mem::size_of::<MONITORINFOEXW>() as u32;
    // SAFETY: `info` is a MONITORINFOEXW with cbSize set, so the device name is written too.
    let ok = unsafe { GetMonitorInfoW(mon, &mut info as *mut MONITORINFOEXW as *mut MONITORINFO) }.as_bool();
    let device = if ok {
        let len = info.szDevice.iter().position(|c| *c == 0).unwrap_or(info.szDevice.len());
        String::from_utf16_lossy(&info.szDevice[..len])
    } else {
        String::new()
    };
    Primary { handle: mon.0 as isize, device }
}

/// The read-only HDR state is re-read when the displays changed (an HDR switch is a display
/// change), else every `every`: [`SAFETY_EVERY`] while the messages are heard, 5 s if not.
pub fn hdr_refresh_due(changed: bool, heard: bool, since: Duration) -> bool {
    // timer-ok: the timed read is a safety net (or the fallback with no session window).
    changed || since >= if heard { SAFETY_EVERY } else { CHECK_EVERY }
}

impl Controller {
    /// Follow the primary monitor (every pump pass; reads on a display change, else at most
    /// every [`SAFETY_EVERY`]). `changed`: Windows said the displays changed since the last pass.
    pub(crate) fn monitor_tick(&mut self, now: Instant, changed: bool) {
        self.primary.hear_changes(crate::record_session_win::hears_display_changes());
        if changed {
            self.primary.poke();
        }
        let Some(change) = self.primary.poll(now, read_primary) else { return };
        if change.moved {
            log!("record: primary monitor is now {} ({:#x})", change.now.device, change.now.handle);
        }
        self.target_monitor = change.now.handle;
        crate::watcher::set_target_monitor(change.now.handle);
        if restarts_desktop(&change, self.planner.applied()) {
            log!("record: the primary monitor changed; restarting the desktop capture");
            self.request_engine_restart();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::record_target::Target;

    fn primary(handle: isize, device: &str) -> Primary {
        Primary { handle, device: device.into() }
    }

    #[test]
    fn the_primary_is_re_read_every_5_s_and_a_change_is_reported_once() {
        let t0 = Instant::now();
        let mut w = PrimaryWatch::new(primary(1, r"\\.\DISPLAY1"), t0);
        let never = || -> Primary { panic!("read before CHECK_EVERY") };
        assert_eq!(w.poll(t0 + CHECK_EVERY - Duration::from_millis(1), never), None, "cheap: not every tick");
        assert_eq!(w.poll(t0 + CHECK_EVERY, || primary(1, r"\\.\DISPLAY1")), None, "unchanged");
        let at = t0 + CHECK_EVERY * 2;
        assert_eq!(w.poll(at, || primary(2, r"\\.\DISPLAY2")), None, "seen once: it settles first");
        let moved = w.poll(at + SETTLE, || primary(2, r"\\.\DISPLAY2"));
        assert_eq!(moved, Some(PrimaryChange { now: primary(2, r"\\.\DISPLAY2"), moved: true }), "a new primary display");
        assert_eq!(w.poll(at + SETTLE + CHECK_EVERY, || primary(2, r"\\.\DISPLAY2")), None, "reported once");
        let at = at + SETTLE + CHECK_EVERY * 2;
        assert_eq!(w.poll(at, || primary(3, r"\\.\DISPLAY2")), None);
        let rehandled = w.poll(at + SETTLE, || primary(3, r"\\.\DISPLAY2"));
        assert_eq!(rehandled, Some(PrimaryChange { now: primary(3, r"\\.\DISPLAY2"), moved: false }), "hot-plug: same display, new handle");
    }

    #[test]
    fn a_display_port_drop_is_settled_before_anything_restarts() {
        let t0 = Instant::now();
        let ms = Duration::from_millis;
        let d1 = || primary(1, r"\\.\DISPLAY1");
        let d2 = || primary(2, r"\\.\DISPLAY2");
        assert_eq!(SETTLE, Duration::from_secs(2));
        // The link drops: the other display is primary for a moment, then the first one is back.
        let mut w = PrimaryWatch::new(d1(), t0);
        assert_eq!(w.poll(t0 + CHECK_EVERY, d2), None, "seen once: no restart yet");
        let early = || -> Primary { panic!("re-read before SETTLE") };
        assert_eq!(w.poll(t0 + CHECK_EVERY + SETTLE - ms(1), early), None);
        assert_eq!(w.poll(t0 + CHECK_EVERY + SETTLE, d1), None, "it came back: no restart at all");
        assert_eq!(w.poll(t0 + CHECK_EVERY * 2 + SETTLE, d1), None, "and nothing later either");
        // A third reading while settling starts the wait over.
        let mut w = PrimaryWatch::new(d1(), t0);
        assert_eq!(w.poll(t0 + CHECK_EVERY, d2), None);
        assert_eq!(w.poll(t0 + CHECK_EVERY + SETTLE, || primary(3, r"\\.\DISPLAY3")), None, "still moving");
        let settled = w.poll(t0 + CHECK_EVERY + SETTLE * 2, || primary(3, r"\\.\DISPLAY3"));
        assert_eq!(settled, Some(PrimaryChange { now: primary(3, r"\\.\DISPLAY3"), moved: true }), "one restart, for where it ended");
    }

    #[test]
    fn a_display_change_is_read_at_once_and_the_timer_is_only_a_safety_net() {
        let t0 = Instant::now();
        let s = Duration::from_secs;
        let d1 = || primary(1, r"\.\DISPLAY1");
        let d2 = || primary(2, r"\.\DISPLAY2");
        let never = || -> Primary { panic!("read with no change and no safety read due") };
        let mut w = PrimaryWatch::new(d1(), t0);
        w.hear_changes(true);
        assert_eq!(w.poll(t0 + CHECK_EVERY * 2, never), None, "no 5 s reads while changes are heard");
        assert_eq!(w.deadline(), None, "nothing settling: nothing to wake for");
        w.poke();
        let at = t0 + s(11);
        assert_eq!(w.poll(at, d2), None, "WM_DISPLAYCHANGE: read now, then it settles");
        assert_eq!(w.deadline(), Some(at + SETTLE), "the pump wakes to confirm it");
        w.poke(); // the same reconfigure's second message
        assert_eq!(w.poll(at + s(1), never), None, "a poke never cuts the settle short");
        let moved = w.poll(at + SETTLE, d2);
        assert_eq!(moved, Some(PrimaryChange { now: d2(), moved: true }));
        assert_eq!(w.poll(at + SETTLE + SAFETY_EVERY - s(1), never), None);
        assert_eq!(w.poll(at + SETTLE + SAFETY_EVERY, d2), None, "the safety read");
        assert!(hdr_refresh_due(true, true, Duration::ZERO), "HDR: a display change");
        assert!(!hdr_refresh_due(false, true, CHECK_EVERY), "no 5 s HDR reads while heard");
        assert!(hdr_refresh_due(false, false, CHECK_EVERY), "the old 5 s with no session window");
    }

    #[test]
    fn only_a_desktop_capture_restarts_for_a_new_primary() {
        let moved = PrimaryChange { now: primary(2, "B"), moved: true };
        let rehandled = PrimaryChange { now: primary(2, "A"), moved: false };
        let game = EngineTarget::Window(Target { hwnd: 1, pid: 7, name: "G".into(), exe: "G.exe".into() });
        assert!(restarts_desktop(&moved, &EngineTarget::Desktop), "the desktop capture follows the new primary");
        assert!(!restarts_desktop(&rehandled, &EngineTarget::Desktop), "same display: the capture is fine");
        assert!(!restarts_desktop(&moved, &game), "a game capture follows its window");
        assert!(!restarts_desktop(&moved, &EngineTarget::Off));
    }
}
