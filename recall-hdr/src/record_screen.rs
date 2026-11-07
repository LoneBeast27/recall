//! Whether the player can see the recorded game now (`record_capture_watch`): a game that
//! is minimized or behind another window, a locked session (the lock screen, UAC and
//! Ctrl+Alt+Del switch input to the secure desktop) or a display that power management
//! turned off sends no frames, so an encoder still "starting" then is not stuck.
//!
//! The engine's Desktop Duplication has the same check (`recall` `capture/dda_desktop.rs`);
//! that is another crate, so this is the controller's thin copy: `OpenInputDesktop` at
//! most once a second ([`Throttle`]) and the `GUID_CONSOLE_DISPLAY_STATE` power notification.

use std::ffi::c_void;
use std::sync::atomic::{AtomicU8, Ordering::Relaxed};
use std::sync::{Mutex, Once};
use std::time::{Duration, Instant};

use windows::core::GUID;
use windows::Win32::Foundation::{HWND, RECT};
use windows::Win32::Graphics::Gdi::{GetMonitorInfoW, MonitorFromWindow, MONITORINFO, MONITOR_DEFAULTTONEAREST};
use windows::Win32::UI::WindowsAndMessaging::{
    GetForegroundWindow, GetWindowLongPtrW, GetWindowRect, GetWindowThreadProcessId, IsIconic, GWL_STYLE, WS_CAPTION,
    WS_POPUP,
};

use crate::log;
use crate::record_target::Target;

/// One look at the target and the screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Sight {
    /// The foreground window is the target's own window or one of its
    /// process's (a UWP target's frame counts too: [`front_is`]).
    pub in_front: bool,
    pub iconic: bool,
    /// The input desktop is not the user's (lock screen, UAC, Ctrl+Alt+Del).
    pub locked: bool,
    pub display_off: bool,
    /// The window's frame (its [`WindowMode`]).
    pub frame: Frame,
}

/// Fullscreen (exclusive or borderless) or in a window: what the player switches in the
/// game's video settings. A resize or a move is not a mode change.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WindowMode {
    Windowed,
    Fullscreen,
}

/// A window's style bits, its rect and its monitor's rect (left, top, right, bottom).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Frame {
    pub style: u32,
    pub rect: [i32; 4],
    pub monitor: [i32; 4],
}

impl Frame {
    /// A 2560x1440 borderless window on a 2560x1440 monitor.
    pub const FULLSCREEN: Self = Self { style: WS_POPUP.0, rect: [0, 0, 2560, 1440], monitor: [0, 0, 2560, 1440] };

    /// Fullscreen: the window covers its whole monitor and has no title bar. A maximized
    /// window also covers it (its borders hang off the edges) but keeps its title bar.
    pub fn mode(self) -> WindowMode {
        let [l, t, r, b] = self.rect;
        let [ml, mt, mr, mb] = self.monitor;
        let covers = l <= ml && t <= mt && r >= mr && b >= mb;
        let titled = self.style & WS_CAPTION.0 == WS_CAPTION.0;
        if covers && !titled { WindowMode::Fullscreen } else { WindowMode::Windowed }
    }
}

impl Sight {
    /// In front, not minimized, on the user's desktop with the display on.
    pub const SHOWN: Self = Self { in_front: true, iconic: false, locked: false, display_off: false, frame: Frame::FULLSCREEN };

    /// The game can be sending frames now.
    pub fn shown(self) -> bool {
        self.in_front && !self.iconic && !self.locked && !self.display_off
    }

    pub fn mode(self) -> WindowMode {
        self.frame.mode()
    }
}

#[link(name = "user32")]
unsafe extern "system" {
    fn OpenInputDesktop(flags: u32, inherit: i32, access: u32) -> isize;
    fn CloseDesktop(desktop: isize) -> i32;
}

#[repr(C)]
struct Subscribe {
    callback: unsafe extern "system" fn(*const c_void, u32, *const c_void) -> u32,
    context: *mut c_void,
}

#[link(name = "powrprof")]
unsafe extern "system" {
    fn PowerSettingRegisterNotification(setting: *const GUID, flags: u32, recipient: *const Subscribe, handle: *mut *mut c_void) -> u32;
}

const DESKTOP_SWITCHDESKTOP: u32 = 0x0100;
const DEVICE_NOTIFY_CALLBACK: u32 = 2;
const PBT_POWERSETTINGCHANGE: u32 = 0x8013;
/// `GUID_CONSOLE_DISPLAY_STATE`: data 0 off, 1 on, 2 dimmed.
const CONSOLE_DISPLAY_STATE: GUID = GUID::from_u128(0x6fe69556_704a_47a0_8f24_c28d936fda47);

/// The console display state from the power notification (1 = on until told otherwise).
static DISPLAY: AtomicU8 = AtomicU8::new(1);
static WATCH: Once = Once::new();

/// How often the input desktop is looked at.
const DESKTOP_EVERY: Duration = Duration::from_secs(1);
static LOCKED: Mutex<Throttle> = Mutex::new(Throttle::new(DESKTOP_EVERY));

/// A probe asked at most once per `every`; the last answer in between.
#[derive(Debug)]
pub struct Throttle {
    every: Duration,
    last: Option<(Instant, bool)>,
}

impl Throttle {
    pub const fn new(every: Duration) -> Self {
        Self { every, last: None }
    }

    pub fn get(&mut self, now: Instant, probe: impl FnOnce() -> bool) -> bool {
        match self.last {
            Some((at, v)) if now.saturating_duration_since(at) < self.every => v,
            _ => {
                let v = probe();
                self.last = Some((now, v));
                v
            }
        }
    }
}

/// The target and the screen now. The display watch is registered on the first call.
pub fn sight(t: &Target) -> Sight {
    WATCH.call_once(watch_display);
    let hwnd = HWND(t.hwnd as *mut _);
    let mut front = 0u32;
    let mut rect = RECT::default();
    let mut monitor = MONITORINFO { cbSize: size_of::<MONITORINFO>() as u32, ..Default::default() };
    // SAFETY: plain queries; each tolerates a stale handle.
    let (front_hwnd, iconic, style) = unsafe {
        let front_hwnd = GetForegroundWindow();
        GetWindowThreadProcessId(front_hwnd, Some(&mut front));
        let _ = GetWindowRect(hwnd, &mut rect);
        let _ = GetMonitorInfoW(MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST), &mut monitor);
        (front_hwnd, IsIconic(hwnd).as_bool(), GetWindowLongPtrW(hwnd, GWL_STYLE) as u32)
    };
    let edges = |r: RECT| [r.left, r.top, r.right, r.bottom];
    let locked = !LOCKED.lock().unwrap_or_else(|p| p.into_inner()).get(Instant::now(), input_is_users);
    Sight {
        in_front: front_is(t, front_hwnd.0 as isize, front),
        iconic,
        locked,
        display_off: DISPLAY.load(Relaxed) == 0,
        frame: Frame { style, rect: edges(rect), monitor: edges(monitor.rcMonitor) },
    }
}

/// The foreground window (`front_hwnd`, owned by `front_pid`) is the target's:
/// its own window, or any window of its process. A UWP target's pid is the
/// hosted app's (G6) while the foreground is its frame, owned by
/// `ApplicationFrameHost`: so the frame's hwnd is compared too.
pub(crate) fn front_is(t: &Target, front_hwnd: isize, front_pid: u32) -> bool {
    (front_hwnd != 0 && front_hwnd == t.hwnd) || (front_pid != 0 && front_pid == t.pid)
}

/// Opening the input desktop works (it fails on the Winlogon desktop).
fn input_is_users() -> bool {
    // SAFETY: the handle is closed right away.
    let desktop = unsafe { OpenInputDesktop(0, 0, DESKTOP_SWITCHDESKTOP) };
    if desktop == 0 {
        return false;
    }
    unsafe { CloseDesktop(desktop) };
    true
}

/// Display on/off changes (Windows sends the current state at once); lives for the process.
fn watch_display() {
    let subscribe: &'static Subscribe = Box::leak(Box::new(Subscribe { callback: on_power, context: std::ptr::null_mut() }));
    let mut handle = std::ptr::null_mut();
    // SAFETY: `subscribe` is leaked, so it outlives the registration.
    let err = unsafe { PowerSettingRegisterNotification(&CONSOLE_DISPLAY_STATE, DEVICE_NOTIFY_CALLBACK, subscribe, &mut handle) };
    if err != 0 {
        log!("record: display power watch unavailable (error {err}); the display counts as on");
    }
}

unsafe extern "system" fn on_power(_context: *const c_void, kind: u32, setting: *const c_void) -> u32 {
    if kind == PBT_POWERSETTINGCHANGE && !setting.is_null() {
        // POWERBROADCAST_SETTING: GUID (16 bytes), DataLength (u32), then the data.
        let base = setting.cast::<u8>();
        let guid = unsafe { base.cast::<GUID>().read_unaligned() };
        let len = unsafe { base.add(16).cast::<u32>().read_unaligned() };
        if guid == CONSOLE_DISPLAY_STATE && len >= 4 {
            let state = unsafe { base.add(20).cast::<u32>().read_unaligned() };
            DISPLAY.store(u8::from(state != 0), Relaxed);
        }
    }
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    const TITLED: u32 = WS_CAPTION.0 | 0x0004_0000; // + WS_THICKFRAME

    #[test]
    fn a_uwp_target_is_in_front_when_its_frame_is() {
        // G6: the target is the hosted app (pid 7) in the frame 0x50, owned by
        // ApplicationFrameHost (pid 3).
        let uwp = Target { hwnd: 0x50, pid: 7, name: "Game".into(), exe: "Game.exe".into() };
        assert!(front_is(&uwp, 0x50, 3), "its frame is the foreground");
        assert!(!front_is(&uwp, 0x60, 3), "another UWP frame");
        let plain = Target { hwnd: 0x70, pid: 9, name: "Game".into(), exe: "Game.exe".into() };
        assert!(front_is(&plain, 0x71, 9), "another window of the game's process");
        assert!(!front_is(&plain, 0x80, 4), "another app");
        assert!(!front_is(&plain, 0, 0), "no foreground");
    }

    #[test]
    fn fullscreen_is_a_window_covering_its_monitor_with_no_title_bar() {
        let on = |style, rect| Frame { style, rect, monitor: [0, 0, 2560, 1440] }.mode();
        assert_eq!(on(WS_POPUP.0, [0, 0, 2560, 1440]), WindowMode::Fullscreen, "borderless / exclusive");
        assert_eq!(on(0, [0, 0, 2560, 1440]), WindowMode::Fullscreen, "no style bits at all");
        assert_eq!(on(TITLED, [100, 100, 1700, 1000]), WindowMode::Windowed);
        assert_eq!(on(TITLED, [100, 100, 2020, 1180]), WindowMode::Windowed, "resized: still windowed");
        assert_eq!(on(TITLED, [-8, -8, 2568, 1448]), WindowMode::Windowed, "maximized keeps its title bar");
        assert_eq!(on(WS_POPUP.0, [0, 0, 1920, 1080]), WindowMode::Windowed, "a borderless window smaller than the monitor");
        let second = Frame { style: WS_POPUP.0, rect: [2560, 0, 4480, 1080], monitor: [2560, 0, 4480, 1080] };
        assert_eq!(second.mode(), WindowMode::Fullscreen, "on a second monitor");
    }

    #[test]
    fn the_input_desktop_is_looked_at_at_most_once_a_second() {
        let t0 = Instant::now();
        let mut t = Throttle::new(DESKTOP_EVERY);
        let mut calls = 0;
        for ms in (0..=3000).step_by(100) {
            t.get(t0 + Duration::from_millis(ms), || {
                calls += 1;
                true
            });
        }
        assert_eq!(calls, 4, "0, 1, 2, 3 s");
    }
}
