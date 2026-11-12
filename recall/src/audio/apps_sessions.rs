//! When to look for closed apps again: Windows says so when any process starts audio
//! (`IAudioSessionManager2::RegisterSessionNotification` on every active render endpoint,
//! `IAudioSessionNotification::OnSessionCreated`), and when an endpoint comes or goes
//! (`notify.rs`), so a new endpoint is watched too. Nothing here opens a process: the scanner
//! lists processes (`snapshot_processes`) only after such a notice, and only while an app is
//! closed. An app that runs without making a sound has nothing to capture until it does.

use std::collections::HashSet;
use std::time::Duration;

use windows::core::implement;
use windows::Win32::Media::Audio::{
    eRender, IAudioSessionControl, IAudioSessionManager2, IAudioSessionNotification, IAudioSessionNotification_Impl, IMMDeviceEnumerator, MMDeviceEnumerator,
    DEVICE_STATE_ACTIVE,
};
use windows::Win32::System::Com::{CoCreateInstance, CLSCTX_ALL};

use super::super::wake::{TrackWake, Waker};

/// timer-ok: a session that starts on an endpoint between its arrival and its registration
/// here (or on one whose session manager could not be activated) is never announced; this
/// rare rescan still finds that app.
pub(super) const SAFETY_RESCAN: Duration = Duration::from_secs(60);

#[implement(IAudioSessionNotification)]
struct SessionNotice(Waker);

impl IAudioSessionNotification_Impl for SessionNotice_Impl {
    fn OnSessionCreated(&self, _session: windows::core::Ref<'_, IAudioSessionControl>) -> windows::core::Result<()> {
        // Only a wake: Windows forbids registering or releasing from this callback, and the
        // scanner's process list says whose session it is.
        self.0.wake();
        Ok(())
    }
}

/// The session notices of every active render endpoint. Lives on the scanner thread (COM MTA).
pub(super) struct Sessions {
    enumerator: IMMDeviceEnumerator,
    notice: IAudioSessionNotification,
    /// The endpoint ids watched, and their managers (kept alive: releasing one stops its notices).
    watched: HashSet<String>,
    managers: Vec<IAudioSessionManager2>,
}

impl Sessions {
    pub(super) fn register(wake: &TrackWake) -> Result<Self, String> {
        let enumerator: IMMDeviceEnumerator = unsafe { CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL) }.map_err(|e| format!("device enumerator: {e}"))?;
        let notice: IAudioSessionNotification = SessionNotice(wake.waker()).into();
        let mut sessions = Sessions { enumerator, notice, watched: HashSet::new(), managers: Vec::new() };
        sessions.refresh();
        if sessions.managers.is_empty() {
            return Err("no render endpoint accepted session notices".into());
        }
        Ok(sessions)
    }

    /// Watch every active render endpoint not watched yet (after a device notice). Returns how
    /// many were added.
    pub(super) fn refresh(&mut self) -> usize {
        let Ok(devices) = (unsafe { self.enumerator.EnumAudioEndpoints(eRender, DEVICE_STATE_ACTIVE) }) else { return 0 };
        let count = unsafe { devices.GetCount() }.unwrap_or(0);
        let mut added = 0;
        for i in 0..count {
            let Ok(device) = (unsafe { devices.Item(i) }) else { continue };
            let Some(id) = (unsafe { device.GetId() }).ok().and_then(|p| unsafe { p.to_string() }.ok()) else { continue };
            if self.watched.contains(&id) {
                continue;
            }
            let Ok(manager) = (unsafe { device.Activate::<IAudioSessionManager2>(CLSCTX_ALL, None) }) else { continue };
            if unsafe { manager.RegisterSessionNotification(&self.notice) }.is_err() {
                continue;
            }
            // Notices begin only once the session list was asked for (IAudioSessionManager2 docs).
            let _ = unsafe { manager.GetSessionEnumerator() };
            self.watched.insert(id);
            self.managers.push(manager);
            added += 1;
        }
        added
    }
}

impl Drop for Sessions {
    fn drop(&mut self) {
        for manager in &self.managers {
            let _ = unsafe { manager.UnregisterSessionNotification(&self.notice) };
        }
    }
}
