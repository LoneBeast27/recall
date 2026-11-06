//! Thin Win32 glue for `record_power`: registers for suspend/resume notices
//! (`PowerRegisterSuspendResumeNotification` with a callback, so no window is
//! needed) and applies the planner's actions on the pump thread.
//!
//! The callback runs on a system thread. It queues the notice; on suspend it
//! waits up to [`SUSPEND_WAIT`] for the pump to act on it, so the long
//! recording's `record_stop` is sent before the PC sleeps.

use std::ffi::c_void;
use std::sync::{Condvar, Mutex};
use std::time::Duration;

use crate::log;
use crate::record_app::Controller;
use crate::record_power::{PowerAction, PowerEvent, PBT_SUSPEND};

/// How long a suspend notice waits for the pump (the pump wakes every 50 ms).
const SUSPEND_WAIT: Duration = Duration::from_secs(2);
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

/// Notices not yet taken by the pump, how many were posted, how many were acted on.
struct Queue {
    pending: Vec<u32>,
    posted: u64,
    served: u64,
}

static QUEUE: Mutex<Queue> = Mutex::new(Queue { pending: Vec::new(), posted: 0, served: 0 });
static SERVED: Condvar = Condvar::new();

#[link(name = "powrprof")]
extern "system" {
    fn PowerRegisterSuspendResumeNotification(flags: u32, recipient: *const c_void, handle: *mut *mut c_void) -> u32;
}

unsafe extern "system" fn on_power(_context: *const c_void, kind: u32, _setting: *const c_void) -> u32 {
    let mut queue = QUEUE.lock().unwrap_or_else(|p| p.into_inner());
    queue.pending.push(kind);
    queue.posted += 1;
    let ticket = queue.posted;
    if kind == PBT_SUSPEND {
        let _ = SERVED.wait_timeout_while(queue, SUSPEND_WAIT, |q| q.served < ticket);
    }
    0 // ERROR_SUCCESS
}

/// Register once at controller start-up. The registration lives as long as the process.
pub(crate) fn register() {
    let mut handle: *mut c_void = std::ptr::null_mut();
    let recipient = &PARAMS as *const SubscribeParams as *const c_void;
    let code = unsafe { PowerRegisterSuspendResumeNotification(DEVICE_NOTIFY_CALLBACK, recipient, &mut handle) };
    if code == 0 {
        log!("record: listening for sleep and resume");
    } else {
        log!("record: sleep/resume notices unavailable (error {code}); a resume keeps the old engine");
    }
}

/// The notices queued since the last call, and the ticket that marks them served.
fn take() -> (Vec<u32>, u64) {
    let mut queue = QUEUE.lock().unwrap_or_else(|p| p.into_inner());
    (std::mem::take(&mut queue.pending), queue.posted)
}

fn mark_served(ticket: u64) {
    let mut queue = QUEUE.lock().unwrap_or_else(|p| p.into_inner());
    queue.served = queue.served.max(ticket);
    SERVED.notify_all();
}

impl Controller {
    /// Pump step: act on the sleep/resume notices that arrived since the last tick.
    pub(crate) fn power_tick(&mut self, now: std::time::Instant) {
        let (kinds, ticket) = take();
        if kinds.is_empty() {
            return;
        }
        for event in kinds.into_iter().filter_map(PowerEvent::from_pbt) {
            let recording = self.activity.is_recording();
            log!("record: power notice {event:?} (long recording running: {recording})");
            for action in self.power.on_event(event, recording) {
                self.apply_power_action(action, now);
            }
        }
        mark_served(ticket);
    }

    fn apply_power_action(&mut self, action: PowerAction, now: std::time::Instant) {
        match action {
            PowerAction::StopRecording => {
                log!("record: sleeping -- stopping the long recording");
                self.supervisor.send_engine_line(r#"{"cmd":"record_stop"}"#);
            }
            PowerAction::RestartEngine => {
                log!("record: resumed from sleep -- restarting the engine (fresh replay buffer)");
                self.restart_engine_for_settings();
            }
            PowerAction::Reprobe => {
                self.last_fed = None;
                self.probe_foreground(now, false);
            }
        }
    }
}
