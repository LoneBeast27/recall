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
use windows::Win32::UI::WindowsAndMessaging::{
    GetForegroundWindow, GetWindowLongPtrW, GetWindowRect, GetWindowThreadProcessId, IsIconic, GWL_STYLE, WS_CAPTION,
    WS_POPUP, WS_THICKFRAME,
};

use crate::log;
use crate::record_target::Target;

/// One look at the target and the screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Sight {
    /// The foreground window belongs to the target's process.
    pub in_front: bool,
    pub iconic: bool,
    /// The input desktop is not the user's (lock screen, UAC, Ctrl+Alt+Del).
    pub locked: bool,
    pub display_off: bool,
    /// The window mode: frame style bits and size (a change = windowed / borderless /
    /// fullscreen switched, or a new size).
    pub mode: u64,
}

impl Sight {
    /// In front, not minimized, on the user's desktop with the display on.
    pub const SHOWN: Self = Self { in_front: true, iconic: false, locked: false, display_off: false, mode: 0 };

    /// The game can be sending frames now.
    pub fn shown(self) -> bool {
        self.in_front && !self.iconic && !self.locked && !self.display_off
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
    // SAFETY: plain queries; each tolerates a stale handle.
    let (iconic, style) = unsafe {
        GetWindowThreadProcessId(GetForegroundWindow(), Some(&mut front));
        let _ = GetWindowRect(hwnd, &mut rect);
        (IsIconic(hwnd).as_bool(), GetWindowLongPtrW(hwnd, GWL_STYLE) as u32 & (WS_CAPTION.0 | WS_THICKFRAME.0 | WS_POPUP.0))
    };
    let size = ((rect.right - rect.left) as u32 as u64) << 16 | (rect.bottom - rect.top) as u16 as u64;
    let locked = !LOCKED.lock().unwrap_or_else(|p| p.into_inner()).get(Instant::now(), input_is_users);
    Sight {
        in_front: front != 0 && front == t.pid,
        iconic,
        locked,
        display_off: DISPLAY.load(Relaxed) == 0,
        mode: (style as u64) << 32 | size,
    }
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
