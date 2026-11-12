//! When to look for closed apps again: Windows says so when any process starts audio
//! (`IAudioSessionManager2::RegisterSessionNotification` on every active render endpoint,
//! `IAudioSessionNotification::OnSessionCreated`), and when an endpoint comes or goes
//! (`notify.rs`), so a new endpoint is watched too. Nothing here opens a process: the scanner
//! lists processes (`snapshot_processes`) only after such a notice, and only while an app is
//! closed. An app that runs without making a sound has nothing to capture until it does.

use std::collections::{HashMap, HashSet};
use std::time::Duration;

use windows::core::implement;
use windows::Win32::Media::Audio::{
    eRender, IAudioSessionControl, IAudioSessionManager2, IMMDevice, IAudioSessionNotification, IAudioSessionNotification_Impl, IMMDeviceEnumerator, MMDeviceEnumerator,
    DEVICE_STATE_ACTIVE,
};
use windows::Win32::System::Com::{CoCreateInstance, CoTaskMemFree, CLSCTX_ALL};

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
    /// The endpoints watched, by id, with their managers (kept alive: releasing one stops its
    /// notices). A removed endpoint leaves, so it is watched again when it comes back.
    watched: HashMap<String, IAudioSessionManager2>,
}

impl Sessions {
    pub(super) fn register(wake: &TrackWake) -> Result<Self, String> {
        let enumerator: IMMDeviceEnumerator = unsafe { CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL) }.map_err(|e| format!("device enumerator: {e}"))?;
        let notice: IAudioSessionNotification = SessionNotice(wake.waker()).into();
        let mut sessions = Sessions { enumerator, notice, watched: HashMap::new() };
        sessions.refresh();
        if sessions.watched.is_empty() {
            return Err("no render endpoint accepted session notices".into());
        }
        Ok(sessions)
    }

    /// After a device notice: stop watching endpoints that are gone (or no longer active) and
    /// watch every active one not watched yet. Returns (dropped, added).
    pub(super) fn refresh(&mut self) -> (usize, usize) {
        let Ok(devices) = (unsafe { self.enumerator.EnumAudioEndpoints(eRender, DEVICE_STATE_ACTIVE) }) else { return (0, 0) };
        let count = unsafe { devices.GetCount() }.unwrap_or(0);
        let active: Vec<(String, IMMDevice)> = (0..count)
            .filter_map(|i| unsafe { devices.Item(i) }.ok())
            .filter_map(|device| endpoint_id(&device).map(|id| (id, device)))
            .collect();
        let ids: Vec<String> = active.iter().map(|(id, _)| id.clone()).collect();
        let (gone, new) = plan(self.watched.keys(), &ids);
        for id in &gone {
            if let Some(manager) = self.watched.remove(id) {
                let _ = unsafe { manager.UnregisterSessionNotification(&self.notice) };
            }
        }
        let mut added = 0;
        for (id, device) in active.into_iter().filter(|(id, _)| new.contains(id)) {
            let Ok(manager) = (unsafe { device.Activate::<IAudioSessionManager2>(CLSCTX_ALL, None) }) else { continue };
            if unsafe { manager.RegisterSessionNotification(&self.notice) }.is_err() {
                continue;
            }
            // Notices begin only once the session list was asked for (IAudioSessionManager2 docs).
            let _ = unsafe { manager.GetSessionEnumerator() };
            self.watched.insert(id, manager);
            added += 1;
        }
        (gone.len(), added)
    }
}

/// The endpoint's id (the COM string freed after the copy).
fn endpoint_id(device: &IMMDevice) -> Option<String> {
    let raw = unsafe { device.GetId() }.ok()?;
    let id = unsafe { raw.to_string() }.ok();
    unsafe { CoTaskMemFree(Some(raw.0 as *const std::ffi::c_void)) };
    id
}

/// Which watched ids are no longer active, and which active ids are not watched yet.
fn plan<'a>(watched: impl Iterator<Item = &'a String>, active: &[String]) -> (Vec<String>, HashSet<String>) {
    let watched: HashSet<&String> = watched.collect();
    let gone = watched.iter().filter(|id| !active.contains(id)).map(|id| (*id).clone()).collect();
    let new = active.iter().filter(|id| !watched.contains(id)).cloned().collect();
    (gone, new)
}

impl Drop for Sessions {
    fn drop(&mut self) {
        for manager in self.watched.values() {
            let _ = unsafe { manager.UnregisterSessionNotification(&self.notice) };
        }
    }
}

#[cfg(test)]
mod tests {
    use super::plan;
    use std::collections::HashSet;

    /// Gate P2-3: a headset removed and plugged in again is watched again at once.
    #[test]
    fn a_removed_endpoint_leaves_and_comes_back_as_new() {
        let s = |v: &[&str]| v.iter().map(|x| x.to_string()).collect::<Vec<String>>();
        let watched = s(&["speakers", "headset"]);
        let (gone, new) = plan(watched.iter(), &s(&["speakers"]));
        assert_eq!((gone, new), (s(&["headset"]), HashSet::new()), "unplugged: dropped");
        let watched = s(&["speakers"]);
        let (gone, new) = plan(watched.iter(), &s(&["speakers", "headset"]));
        assert_eq!((gone, new), (Vec::new(), ["headset".to_string()].into_iter().collect()), "back: watched again");
    }
}
