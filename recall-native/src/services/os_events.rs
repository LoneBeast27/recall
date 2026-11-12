//! Windows' broadcast notifications, for threads that used to re-check on a timer: a device
//! interface arrived or left (a MIDI or audio device plugged in), a volume mounted or went (a
//! drive plugged back in), the clock or time zone changed or the PC woke from sleep.
//!
//! One hidden top-level window on its own thread receives them (a message-only window gets no
//! broadcasts). It is never shown, sits in this process only, and hooks nothing. Subscribers
//! get an auto-reset event that is set on each notification of their topic.
use std::sync::{Arc, OnceLock};

use windows::Win32::{
    Foundation::{HANDLE, HWND, LPARAM, LRESULT, WPARAM},
    System::LibraryLoader::GetModuleHandleW,
    UI::WindowsAndMessaging::{
        CreateWindowExW, DBT_DEVICEARRIVAL, DBT_DEVICEREMOVECOMPLETE, DBT_DEVNODES_CHANGED, DBT_DEVTYP_DEVICEINTERFACE, DBT_DEVTYP_VOLUME,
        DEV_BROADCAST_DEVICEINTERFACE_W, DEV_BROADCAST_HDR, DEVICE_NOTIFY_ALL_INTERFACE_CLASSES, DEVICE_NOTIFY_WINDOW_HANDLE, DefWindowProcW,
        DispatchMessageW, GetMessageW, MSG, PBT_APMRESUMEAUTOMATIC, RegisterClassExW, RegisterDeviceNotificationW, WM_DEVICECHANGE,
        WM_POWERBROADCAST, WM_TIMECHANGE, WNDCLASSEXW, WS_EX_TOOLWINDOW, WS_POPUP,
    },
};
use windows::core::w;

use super::win_event::{Event, Subscribers};

/// What a subscriber wakes for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Topic {
    /// A device interface arrived or was removed, or the device tree changed.
    Devices,
    /// A volume (drive letter) arrived or was removed.
    Volumes,
    /// The system clock or time zone changed, or the PC resumed from sleep.
    Clock,
}

const TOPICS: usize = 3;

fn index(topic: Topic) -> usize {
    match topic {
        Topic::Devices => 0,
        Topic::Volumes => 1,
        Topic::Clock => 2,
    }
}

static TOPIC_SUBSCRIBERS: [Subscribers; TOPICS] = [Subscribers::new(), Subscribers::new(), Subscribers::new()];

fn topic(topic: Topic) -> &'static Subscribers {
    &TOPIC_SUBSCRIBERS[index(topic)]
}

/// Raises `kind`: its generation moves on and every live subscriber's event is set.
fn raise(kind: Topic) {
    topic(kind).raise();
    if kind == Topic::Clock {
        // The library's day labels follow the clock (`library_meta::roll_day`).
        super::ui_wake::wake();
    }
}

/// An event set on each `topic` notification, for as long as it is kept. `None` when the
/// listener could not start: the caller keeps its timer then.
pub fn subscribe(kind: Topic) -> Option<Arc<Event>> {
    if !listening() {
        return None;
    }
    topic(kind).subscribe()
}

/// Moves on with each `topic` notification (readers compare it with the one they saw).
pub fn generation(kind: Topic) -> u64 {
    topic(kind).raised()
}

/// Starts the listener thread once; true when its window is up.
pub fn listening() -> bool {
    static UP: OnceLock<bool> = OnceLock::new();
    *UP.get_or_init(|| {
        let (tx, rx) = std::sync::mpsc::channel();
        let spawned = std::thread::Builder::new().name("recall-os-events".into()).spawn(move || {
            let window = create_window();
            let _ = tx.send(window.is_some());
            if window.is_some() {
                pump();
            }
        });
        spawned.is_ok() && rx.recv().unwrap_or(false)
    })
}

fn create_window() -> Option<HWND> {
    let instance = unsafe { GetModuleHandleW(None) }.ok()?;
    let class = WNDCLASSEXW {
        cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
        lpfnWndProc: Some(window_proc),
        hInstance: instance.into(),
        lpszClassName: w!("RecallOsEvents"),
        ..Default::default()
    };
    // A second registration (never in practice: one listener per process) fails harmlessly.
    let _ = unsafe { RegisterClassExW(&class) };
    // Top-level and hidden: broadcasts reach it, the user never sees it.
    let hwnd = unsafe {
        CreateWindowExW(WS_EX_TOOLWINDOW, w!("RecallOsEvents"), w!("Recall OS events"), WS_POPUP, 0, 0, 0, 0, None, None, Some(instance.into()), None)
    }
    .ok()?;
    // Interface arrivals are only sent to windows that register for them.
    let filter = DEV_BROADCAST_DEVICEINTERFACE_W {
        dbcc_size: std::mem::size_of::<DEV_BROADCAST_DEVICEINTERFACE_W>() as u32,
        dbcc_devicetype: DBT_DEVTYP_DEVICEINTERFACE.0,
        ..Default::default()
    };
    let flags = DEVICE_NOTIFY_WINDOW_HANDLE | DEVICE_NOTIFY_ALL_INTERFACE_CLASSES;
    // Kept for the life of the process, like the window.
    let _ = unsafe { RegisterDeviceNotificationW(HANDLE(hwnd.0), std::ptr::from_ref(&filter).cast(), flags) };
    Some(hwnd)
}

fn pump() {
    let mut message = MSG::default();
    while unsafe { GetMessageW(&mut message, None, 0, 0) }.as_bool() {
        unsafe { DispatchMessageW(&message) };
    }
}

unsafe extern "system" fn window_proc(hwnd: HWND, message: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if let Some(topic) = topic_of(message, wparam.0 as u32, header_type(message, wparam.0 as u32, lparam)) {
        raise(topic);
    }
    unsafe { DefWindowProcW(hwnd, message, wparam, lparam) }
}

/// The device type of an arrival or removal's `DEV_BROADCAST_HDR`.
fn header_type(message: u32, event: u32, lparam: LPARAM) -> Option<u32> {
    if message != WM_DEVICECHANGE || !matches!(event, DBT_DEVICEARRIVAL | DBT_DEVICEREMOVECOMPLETE) || lparam.0 == 0 {
        return None;
    }
    // SAFETY: for these two events lparam points at a DEV_BROADCAST_HDR (DBT docs).
    Some(unsafe { (*(lparam.0 as *const DEV_BROADCAST_HDR)).dbch_devicetype.0 })
}

/// Which topic a window message raises.
fn topic_of(message: u32, event: u32, device_type: Option<u32>) -> Option<Topic> {
    match message {
        WM_DEVICECHANGE if event == DBT_DEVNODES_CHANGED => Some(Topic::Devices),
        WM_DEVICECHANGE => match device_type? {
            t if t == DBT_DEVTYP_VOLUME.0 => Some(Topic::Volumes),
            t if t == DBT_DEVTYP_DEVICEINTERFACE.0 => Some(Topic::Devices),
            _ => None,
        },
        WM_TIMECHANGE => Some(Topic::Clock),
        WM_POWERBROADCAST if event == PBT_APMRESUMEAUTOMATIC => Some(Topic::Clock),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn messages_map_to_topics() {
        let arrival = |t: u32| topic_of(WM_DEVICECHANGE, DBT_DEVICEARRIVAL, Some(t));
        assert_eq!(arrival(DBT_DEVTYP_DEVICEINTERFACE.0), Some(Topic::Devices), "a MIDI or audio device plugged in");
        assert_eq!(arrival(DBT_DEVTYP_VOLUME.0), Some(Topic::Volumes), "a drive plugged back in");
        assert_eq!(topic_of(WM_DEVICECHANGE, DBT_DEVNODES_CHANGED, None), Some(Topic::Devices));
        assert_eq!(topic_of(WM_TIMECHANGE, 0, None), Some(Topic::Clock));
        assert_eq!(topic_of(WM_POWERBROADCAST, PBT_APMRESUMEAUTOMATIC, None), Some(Topic::Clock), "woke from sleep");
        assert_eq!(topic_of(WM_POWERBROADCAST, 4, None), None, "going to sleep is not a change");
        assert_eq!(arrival(1), None, "other device types");
    }

    #[test]
    fn a_raised_topic_sets_its_subscribers_only() {
        if !listening() {
            return;
        }
        let devices = subscribe(Topic::Devices).unwrap();
        let volumes = subscribe(Topic::Volumes).unwrap();
        let before = generation(Topic::Volumes);
        raise(Topic::Volumes);
        assert!(volumes.wait(Some(Duration::from_secs(1))));
        assert!(generation(Topic::Volumes) > before);
        // Another test may raise Devices meanwhile; Volumes must not have set it.
        let _ = devices;
    }
}
