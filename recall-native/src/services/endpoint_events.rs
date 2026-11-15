//! Audio endpoint notifications (`IMMNotificationClient`): the Windows default or the
//! communications device moved, an endpoint was added, removed, enabled or disabled. The level
//! meters wait on these instead of re-checking the default every second and re-opening a missing
//! device every 3 s (`meters`).
//!
//! One registration per process, made on a thread of its own that keeps COM (MTA) alive and
//! otherwise sleeps. Callbacks come on Windows' own threads and only set events.
use std::sync::{Arc, OnceLock};

use windows::Win32::{
    Foundation::PROPERTYKEY,
    Media::Audio::{DEVICE_STATE, EDataFlow, ERole, IMMDeviceEnumerator, IMMNotificationClient, IMMNotificationClient_Impl, MMDeviceEnumerator},
    System::Com::{CLSCTX_ALL, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx},
};
use windows_core::{PCWSTR, implement};

use super::win_event::{Event, Subscribers};

static CHANGES: Subscribers = Subscribers::new();

/// An event set on every endpoint change, for as long as it is kept. `None` when the
/// registration failed: the caller keeps its timer then.
pub fn subscribe() -> Option<Arc<Event>> {
    registered().then(|| CHANGES.subscribe()).flatten()
}

/// Endpoint changes since start.
#[cfg(test)]
fn changes() -> u64 {
    CHANGES.raised()
}

#[implement(IMMNotificationClient)]
struct Client;

impl IMMNotificationClient_Impl for Client_Impl {
    fn OnDeviceStateChanged(&self, _: &PCWSTR, _: DEVICE_STATE) -> windows_core::Result<()> {
        CHANGES.raise();
        Ok(())
    }
    fn OnDeviceAdded(&self, _: &PCWSTR) -> windows_core::Result<()> {
        CHANGES.raise();
        Ok(())
    }
    fn OnDeviceRemoved(&self, _: &PCWSTR) -> windows_core::Result<()> {
        CHANGES.raise();
        Ok(())
    }
    fn OnDefaultDeviceChanged(&self, _: EDataFlow, _: ERole, _: &PCWSTR) -> windows_core::Result<()> {
        CHANGES.raise();
        Ok(())
    }
    /// Names and formats change often and move no meter.
    fn OnPropertyValueChanged(&self, _: &PCWSTR, _: &PROPERTYKEY) -> windows_core::Result<()> {
        Ok(())
    }
}

/// Registers once; true when Windows took the callback.
fn registered() -> bool {
    static UP: OnceLock<bool> = OnceLock::new();
    *UP.get_or_init(|| {
        let (tx, rx) = std::sync::mpsc::channel();
        let spawned = std::thread::Builder::new().name("recall-endpoint-events".into()).spawn(move || {
            let _ = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
            let held = register();
            let _ = tx.send(held.is_some());
            if held.is_some() {
                // Holds the enumerator and the client (and this thread's MTA) for the process.
                loop {
                    std::thread::park();
                }
            }
        });
        spawned.is_ok() && rx.recv().unwrap_or(false)
    })
}

fn register() -> Option<(IMMDeviceEnumerator, IMMNotificationClient)> {
    let enumerator: IMMDeviceEnumerator = unsafe { CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL) }.ok()?;
    let client: IMMNotificationClient = Client.into();
    unsafe { enumerator.RegisterEndpointNotificationCallback(&client) }.ok()?;
    Some((enumerator, client))
}

#[cfg(test)]
mod tests {
    #[test]
    fn a_change_sets_the_subscribers() {
        // Raised by hand: a test never plugs devices in or moves the user's default.
        let Some(event) = super::subscribe() else { return };
        let before = super::changes();
        super::CHANGES.raise();
        assert!(event.wait(Some(std::time::Duration::from_secs(1))));
        assert!(super::changes() > before);
    }
}
