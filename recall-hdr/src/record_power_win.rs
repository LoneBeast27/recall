//! Thin Win32 glue for `record_power`: registers for suspend/resume notices
//! (`PowerRegisterSuspendResumeNotification` with a callback, so no window is
//! needed) and applies the planner's actions on the pump thread.
//!
//! The callback runs on a system thread. It only queues the notice and posts a
//! `WM_NULL` to the pump thread so the pump acts on its next pass; it never waits,
//! so the system's power broadcast is not held up (the suspend stop is
//! best-effort, see `record_power`).

use std::ffi::c_void;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Mutex, MutexGuard};

use windows::Win32::Foundation::{LPARAM, WPARAM};
use windows::Win32::System::Threading::GetCurrentThreadId;
use windows::Win32::UI::WindowsAndMessaging::{PostThreadMessageW, WM_NULL};

use crate::log;
use crate::record_app::Controller;
use crate::awake_clock::Awake;
use crate::record_activity::EngineActivity;
use crate::record_power::{enqueue, PowerAction, PowerPlanner, Registration};
use crate::record_power_host::PowerHost;

/// `DEVICE_NOTIFY_CALLBACK`: the recipient is a `DEVICE_NOTIFY_SUBSCRIBE_PARAMETERS`.
const DEVICE_NOTIFY_CALLBACK: u32 = 2;

type Callback = unsafe extern "system" fn(context: *const c_void, kind: u32, setting: *const c_void) -> u32;

/// `DEVICE_NOTIFY_SUBSCRIBE_PARAMETERS`. Windows keeps the pointer, so it is a static.
#[repr(C)]
struct SubscribeParams {
    callback: Option<Callback>,
    context: *mut c_void,
}
// SAFETY: never written after start-up; Windows only reads it.
unsafe impl Sync for SubscribeParams {}

static PARAMS: SubscribeParams = SubscribeParams { callback: Some(on_power), context: std::ptr::null_mut() };

/// Notices not yet taken by the pump.
static PENDING: Mutex<Vec<u32>> = Mutex::new(Vec::new());
/// The live registration (its handle is given back at shutdown).
static REGISTRATION: Mutex<Registration> = Mutex::new(Registration::new());
/// The pump's thread id, for the wake message; 0 = no pump to wake.
static PUMP_THREAD: AtomicU32 = AtomicU32::new(0);

#[link(name = "powrprof")]
extern "system" {
    fn PowerRegisterSuspendResumeNotification(flags: u32, recipient: *const c_void, handle: *mut *mut c_void) -> u32;
    fn PowerUnregisterSuspendResumeNotification(handle: *mut c_void) -> u32;
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}

unsafe extern "system" fn on_power(_context: *const c_void, kind: u32, _setting: *const c_void) -> u32 {
    let wake = enqueue(&mut lock(&PENDING), kind);
    let pump = PUMP_THREAD.load(Ordering::Acquire);
    if wake && pump != 0 {
        // A failed post only costs latency: the pump also wakes on its own every 50 ms.
        let _ = unsafe { PostThreadMessageW(pump, WM_NULL, WPARAM(0), LPARAM(0)) };
    }
    0 // ERROR_SUCCESS
}

/// Register at controller start-up, on the pump thread. A second call while registered
/// does nothing.
pub(crate) fn register() {
    PUMP_THREAD.store(unsafe { GetCurrentThreadId() }, Ordering::Release);
    let outcome = lock(&REGISTRATION).register(|| {
        let mut handle: *mut c_void = std::ptr::null_mut();
        let recipient = &PARAMS as *const SubscribeParams as *const c_void;
        let code = unsafe { PowerRegisterSuspendResumeNotification(DEVICE_NOTIFY_CALLBACK, recipient, &mut handle) };
        if code == 0 { Ok(handle as usize) } else { Err(code) }
    });
    match outcome {
        Ok(true) => log!("record: listening for sleep and resume"),
        Ok(false) => {}
        Err(code) => log!("record: sleep/resume notices unavailable (error {code}); a resume keeps the old engine"),
    }
}

/// Give the registration back at controller shutdown. Safe to call again, or unregistered.
pub(crate) fn unregister() {
    lock(&REGISTRATION).unregister(|handle| {
        let code = unsafe { PowerUnregisterSuspendResumeNotification(handle as *mut c_void) };
        if code == 0 {
            log!("record: stopped listening for sleep and resume");
        } else {
            log!("record: sleep/resume unregister failed (error {code})");
        }
    });
    PUMP_THREAD.store(0, Ordering::Release);
}

/// The notices queued since the last call.
fn take() -> Vec<u32> {
    std::mem::take(&mut *lock(&PENDING))
}

/// The controller as the host of the sleep/resume rules (`record_power_host::power_tick`,
/// run by the pump before `tick`).
impl PowerHost for Controller {
    fn power_notices(&mut self) -> Vec<u32> {
        take()
    }
    fn power_planner(&mut self) -> &mut PowerPlanner {
        &mut self.power
    }
    fn power_activity(&mut self) -> &mut EngineActivity {
        &mut self.activity
    }
    fn power_awake(&self) -> Awake {
        self.awake()
    }
    fn power_apply(&mut self, action: PowerAction) {
        self.apply_power_action(action, std::time::Instant::now());
    }
    fn power_restart(&mut self) -> bool {
        log!("record: resumed from sleep -- restarting the engine (fresh replay buffer)");
        self.restart_engine(crate::record_restart::RestartReason::Resume) == crate::record_restart::Restarted::Now
    }
    fn power_hub_event(&mut self, line: &str) {
        self.hub.event("engine", line);
    }
    fn power_log(&mut self, line: &str) {
        log!("{line}");
    }
}

impl Controller {
    fn apply_power_action(&mut self, action: PowerAction, now: std::time::Instant) {
        match action {
            PowerAction::StopRecording => {
                log!("record: sleeping -- stopping the long recording");
                // Not a user action: logged (the engine restarts on resume either way).
                if !crate::record_dispatch::automatic_effects(self.send_record_stop()).is_empty() {
                    log!("record: sleeping -- record_stop not sent ({})", crate::record_dispatch::NOT_RESPONDING);
                }
            }
            PowerAction::RestartEngine => {
                PowerHost::power_restart(self); // waits for a drain
            }
            PowerAction::Reprobe => {
                self.last_fed = None;
                self.probe_foreground(now, false);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::record_power::PBT_SUSPEND;
    use std::time::{Duration, Instant};

    #[test]
    fn the_suspend_callback_returns_at_once_with_no_pump() {
        let started = Instant::now();
        let code = unsafe { on_power(std::ptr::null(), PBT_SUSPEND, std::ptr::null()) };
        assert_eq!(code, 0);
        let waited = started.elapsed();
        assert!(waited < Duration::from_millis(500), "the power thread was held for {waited:?}");
    }
}
