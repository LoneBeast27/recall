//! The primary monitor, followed at run time (audit T14). The watcher's
//! managed monitor (`target_monitor`, the `fullscreen_on_target` flag) and the
//! engine's Desktop capture (`Monitor::primary()` at engine start) were read
//! once at start-up, so a hot-plug or a new primary display was not followed.
//! Every [`CHECK_EVERY`] the controller re-reads the primary (two cheap
//! queries): a new handle updates the watcher; a different display as primary
//! restarts the Desktop capture (only while the engine records the desktop; a
//! game capture follows its window).

use std::time::{Duration, Instant};

use windows::Win32::Foundation::POINT;
use windows::Win32::Graphics::Gdi::{GetMonitorInfoW, MonitorFromPoint, MONITORINFO, MONITORINFOEXW, MONITOR_DEFAULTTOPRIMARY};

use crate::log;
use crate::record_app::Controller;
use crate::record_engine::EngineTarget;

/// How often the primary monitor is re-read.
pub const CHECK_EVERY: Duration = Duration::from_secs(5);

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
}

impl PrimaryWatch {
    pub fn new(primary: Primary, now: Instant) -> Self {
        PrimaryWatch { last: primary, checked: now }
    }

    /// Re-read the primary with `read` when [`CHECK_EVERY`] has passed.
    pub fn poll(&mut self, now: Instant, read: impl FnOnce() -> Primary) -> Option<PrimaryChange> {
        if now.duration_since(self.checked) < CHECK_EVERY {
            return None;
        }
        self.checked = now;
        let primary = read();
        if primary == self.last {
            return None;
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

impl Controller {
    /// Follow the primary monitor (every pump tick; reads at most every [`CHECK_EVERY`]).
    pub(crate) fn monitor_tick(&mut self, now: Instant) {
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
        let moved = w.poll(t0 + CHECK_EVERY * 2, || primary(2, r"\\.\DISPLAY2"));
        assert_eq!(moved, Some(PrimaryChange { now: primary(2, r"\\.\DISPLAY2"), moved: true }), "a new primary display");
        assert_eq!(w.poll(t0 + CHECK_EVERY * 3, || primary(2, r"\\.\DISPLAY2")), None, "reported once");
        let rehandled = w.poll(t0 + CHECK_EVERY * 4, || primary(3, r"\\.\DISPLAY2"));
        assert_eq!(rehandled, Some(PrimaryChange { now: primary(3, r"\\.\DISPLAY2"), moved: false }), "hot-plug: same display, new handle");
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
