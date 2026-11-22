//! Whether Recall is in use, for background work. The user, 2026-11-21: "any scenario where
//! resources are being used while recall isn't really in use, we can forfeit most of background
//! tasks until alt tabbing back or app is back in focus".
//!
//! Background filling (thumbnails ahead of the scroll, hover frames, editor media, light copies,
//! clip heads, the pre-encode) runs only while the library window is in front, or while the
//! display is off (nobody is using the PC, so the work takes nothing from anyone). Another app in
//! front or Recall minimised holds it. Both inputs are OS events: the window's activation
//! (`Root::start_polling`) and `GUID_CONSOLE_DISPLAY_STATE` (`os_events`). On-demand work (a
//! click, an open, an export) and the recorder's own writes are never held here.
use std::sync::atomic::{AtomicBool, Ordering};

/// The library window is in front (true until the window reports, and in check sandboxes).
static FRONT: AtomicBool = AtomicBool::new(true);
/// The display is on (true until Windows reports otherwise).
static DISPLAY_ON: AtomicBool = AtomicBool::new(true);

/// The library window came to the front or went behind (minimised counts as behind).
pub fn set_front(front: bool) {
    set(&FRONT, front);
}

/// The display turned on (or dimmed) or off.
pub fn set_display_on(on: bool) {
    set(&DISPLAY_ON, on);
}

fn set(flag: &AtomicBool, value: bool) {
    if flag.swap(value, Ordering::Relaxed) != value && in_use_or_away() {
        // Paused lanes ask their feed again now, not at the 30 s backstop.
        super::lane::wake_paused();
    }
}

/// Recall is in use (in front), or nobody is (display off): background work may run.
fn in_use_or_away() -> bool {
    allowed(FRONT.load(Ordering::Relaxed), DISPLAY_ON.load(Ordering::Relaxed))
}

pub(super) fn allowed(front: bool, display_on: bool) -> bool {
    front || !display_on
}

/// Background work waits: the recorder is recording or saving, or Recall is not in use while
/// someone is at the PC.
pub fn background_held() -> bool {
    super::recorder_busy() || !in_use_or_away()
}

/// Registers for display on/off changes once per process (Windows sends the current state at
/// once). The same power notification the engine's Desktop Duplication watch uses.
pub fn watch_display() {
    static WATCH: std::sync::Once = std::sync::Once::new();
    WATCH.call_once(|| {
        let subscribe: &'static power::Subscribe = Box::leak(Box::new(power::Subscribe { callback: power::on_power, context: std::ptr::null_mut() }));
        let mut handle = std::ptr::null_mut();
        // SAFETY: a leaked (process-long) recipient and an out handle kept for the process.
        let err = unsafe { power::PowerSettingRegisterNotification(&power::CONSOLE_DISPLAY_STATE, power::DEVICE_NOTIFY_CALLBACK, subscribe, &mut handle) };
        if err != 0 {
            eprintln!("Recall native: display power watch unavailable (error {err}); the display counts as on");
        }
    });
}

mod power {
    use std::ffi::c_void;
    use windows::core::GUID;

    pub(super) const DEVICE_NOTIFY_CALLBACK: u32 = 2;
    const PBT_POWERSETTINGCHANGE: u32 = 0x8013;
    /// `GUID_CONSOLE_DISPLAY_STATE`: data 0 off, 1 on, 2 dimmed.
    pub(super) const CONSOLE_DISPLAY_STATE: GUID = GUID::from_u128(0x6fe69556_704a_47a0_8f24_c28d936fda47);

    #[repr(C)]
    pub(super) struct Subscribe {
        pub callback: unsafe extern "system" fn(*const c_void, u32, *const c_void) -> u32,
        pub context: *mut c_void,
    }

    #[link(name = "powrprof")]
    unsafe extern "system" {
        pub(super) fn PowerSettingRegisterNotification(setting: *const GUID, flags: u32, recipient: *const Subscribe, handle: *mut *mut c_void) -> u32;
    }

    pub(super) unsafe extern "system" fn on_power(_context: *const c_void, kind: u32, setting: *const c_void) -> u32 {
        if kind == PBT_POWERSETTINGCHANGE && !setting.is_null() {
            // POWERBROADCAST_SETTING: GUID (16 bytes), DataLength (u32), then the data.
            let base = setting.cast::<u8>();
            let guid = unsafe { base.cast::<GUID>().read_unaligned() };
            let len = unsafe { base.add(16).cast::<u32>().read_unaligned() };
            if guid == CONSOLE_DISPLAY_STATE && len >= 4 {
                let state = unsafe { base.add(20).cast::<u32>().read_unaligned() };
                super::set_display_on(state != 0);
            }
        }
        0
    }
}

/// Why background work waits, for the cache status line (`None` = it runs).
pub fn held_reason() -> Option<&'static str> {
    if super::recorder_busy() {
        Some("paused while recording")
    } else if !in_use_or_away() {
        Some("paused while Recall is in the background")
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::allowed;

    #[test]
    fn background_work_runs_in_front_or_with_the_display_off() {
        assert!(allowed(true, true), "in front");
        assert!(allowed(true, false));
        assert!(allowed(false, false), "nobody at the PC");
        assert!(!allowed(false, true), "another app in front, display on: held");
    }
}
