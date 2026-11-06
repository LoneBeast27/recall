//! Endpoint changes for one capture thread, from Windows (`IMMNotificationClient`): the
//! default endpoint moving, and devices coming, going or changing state. The callbacks only
//! set flags (`Shared`); the capture loop takes them (`capture_loop::Notices`), so nothing on
//! the capture thread polls the endpoints.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use windows::core::{implement, PCWSTR};
use windows::Win32::Foundation::PROPERTYKEY;
use windows::Win32::Media::Audio::{
    eCapture, eConsole, eRender, EDataFlow, ERole, IMMDeviceEnumerator, IMMNotificationClient, IMMNotificationClient_Impl, MMDeviceEnumerator,
    DEVICE_STATE,
};
use windows::Win32::System::Com::{CoCreateInstance, CLSCTX_ALL};

use super::capture_loop::Notices;

/// What the callbacks saw since the capture loop last looked.
struct Shared {
    flow: EDataFlow,
    devices_changed: AtomicBool,
    default: Mutex<Option<String>>,
}

#[implement(IMMNotificationClient)]
struct Client(Arc<Shared>);

impl Client {
    fn devices_changed(&self) {
        self.0.devices_changed.store(true, Ordering::Release);
    }
}

impl IMMNotificationClient_Impl for Client_Impl {
    fn OnDeviceStateChanged(&self, _id: &PCWSTR, _state: DEVICE_STATE) -> windows::core::Result<()> {
        self.devices_changed();
        Ok(())
    }

    fn OnDeviceAdded(&self, _id: &PCWSTR) -> windows::core::Result<()> {
        self.devices_changed();
        Ok(())
    }

    fn OnDeviceRemoved(&self, _id: &PCWSTR) -> windows::core::Result<()> {
        self.devices_changed();
        Ok(())
    }

    /// The role `wasapi::get_default_device` opens (console), for this track's direction. A
    /// null id (no default left) is not recorded: the stream reports its lost device itself.
    fn OnDefaultDeviceChanged(&self, flow: EDataFlow, role: ERole, id: &PCWSTR) -> windows::core::Result<()> {
        if flow == self.0.flow && role == eConsole && !id.is_null() {
            if let (Ok(id), Ok(mut last)) = (unsafe { id.to_string() }, self.0.default.lock()) {
                *last = Some(id);
            }
        }
        Ok(())
    }

    fn OnPropertyValueChanged(&self, _id: &PCWSTR, _key: &PROPERTYKEY) -> windows::core::Result<()> {
        Ok(())
    }
}

/// A registered notification client; unregistered on drop. Made on the capture thread (COM
/// is initialized there); the callbacks run on Windows' own threads.
pub(super) struct EndpointNotices {
    shared: Arc<Shared>,
    enumerator: IMMDeviceEnumerator,
    client: IMMNotificationClient,
}

impl EndpointNotices {
    pub(super) fn register(direction: wasapi::Direction) -> Result<Self, String> {
        let flow = match direction {
            wasapi::Direction::Render => eRender,
            wasapi::Direction::Capture => eCapture,
        };
        let shared = Arc::new(Shared { flow, devices_changed: AtomicBool::new(false), default: Mutex::new(None) });
        let enumerator: IMMDeviceEnumerator = unsafe { CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL) }.map_err(|e| format!("device enumerator: {e}"))?;
        let client: IMMNotificationClient = Client(shared.clone()).into();
        unsafe { enumerator.RegisterEndpointNotificationCallback(&client) }.map_err(|e| format!("register device notices: {e}"))?;
        Ok(EndpointNotices { shared, enumerator, client })
    }
}

impl Drop for EndpointNotices {
    fn drop(&mut self) {
        let _ = unsafe { self.enumerator.UnregisterEndpointNotificationCallback(&self.client) };
    }
}

impl Notices for EndpointNotices {
    fn take_devices_changed(&self) -> bool {
        self.shared.devices_changed.swap(false, Ordering::AcqRel)
    }

    fn take_default(&self) -> Option<String> {
        self.shared.default.lock().ok()?.take()
    }
}
