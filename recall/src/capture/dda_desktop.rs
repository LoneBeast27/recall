//! Whether the screen shows the user's desktop (`dda_logic::Desktop`): the lock screen, UAC
//! and Ctrl+Alt+Del switch input to the secure (Winlogon) desktop, which this process may not
//! open, and a display that power management turned off shows nothing. Desktop Duplication
//! fails in both until the user is back, so neither is a reason to give it up.

use std::ffi::c_void;
use std::sync::atomic::{AtomicU8, Ordering::Relaxed};
use std::sync::Once;

use windows::core::GUID;

use super::dda_logic::Desktop;

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

/// The desktop now. The display watch is registered on the first call (once per process).
pub(super) fn now() -> Desktop {
    WATCH.call_once(watch_display);
    Desktop { input_is_users: input_is_users(), display_on: DISPLAY.load(Relaxed) != 0 }
}

/// The user's desktop takes input: opening the input desktop works (it fails on Winlogon).
fn input_is_users() -> bool {
    let desktop = unsafe { OpenInputDesktop(0, 0, DESKTOP_SWITCHDESKTOP) };
    if desktop == 0 {
        return false;
    }
    unsafe { CloseDesktop(desktop) };
    true
}

/// Registers for display on/off changes (Windows sends the current state at once). The
/// subscription lives for the process.
fn watch_display() {
    let subscribe: &'static Subscribe = Box::leak(Box::new(Subscribe { callback: on_power, context: std::ptr::null_mut() }));
    let mut handle = std::ptr::null_mut();
    let err = unsafe { PowerSettingRegisterNotification(&CONSOLE_DISPLAY_STATE, DEVICE_NOTIFY_CALLBACK, subscribe, &mut handle) };
    if err != 0 {
        eprintln!("[recall] capture: display power watch unavailable (error {err}); the display counts as on");
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
