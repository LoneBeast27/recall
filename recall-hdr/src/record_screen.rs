//! Whether the player can see the recorded game now (`record_capture_watch`): a game that
//! is minimized or behind another window, a locked session (the lock screen, UAC and
//! Ctrl+Alt+Del switch input to the secure desktop) or a display that power management
//! turned off sends no frames, so an encoder still "starting" then is not stuck.
//!
//! The engine's Desktop Duplication has the same check (`recall` `capture/dda_desktop.rs`);
//! that is another crate, so this is the controller's thin copy: one `OpenInputDesktop`
//! per call and the `GUID_CONSOLE_DISPLAY_STATE` power notification.

use std::ffi::c_void;
use std::sync::atomic::{AtomicU8, Ordering::Relaxed};
use std::sync::Once;

use windows::core::GUID;
use windows::Win32::Foundation::HWND;
use windows::Win32::UI::WindowsAndMessaging::{GetForegroundWindow, GetWindowThreadProcessId, IsIconic};

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
}

impl Sight {
    /// In front, not minimized, on the user's desktop with the display on.
    pub const SHOWN: Self = Self { in_front: true, iconic: false, locked: false, display_off: false };

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

/// The target and the screen now. The display watch is registered on the first call.
pub fn sight(t: &Target) -> Sight {
    WATCH.call_once(watch_display);
    let hwnd = HWND(t.hwnd as *mut _);
    let mut front = 0u32;
    // SAFETY: plain queries; both tolerate a stale handle.
    let iconic = unsafe {
        GetWindowThreadProcessId(GetForegroundWindow(), Some(&mut front));
        IsIconic(hwnd).as_bool()
    };
    Sight { in_front: front != 0 && front == t.pid, iconic, locked: !input_is_users(), display_off: DISPLAY.load(Relaxed) == 0 }
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
