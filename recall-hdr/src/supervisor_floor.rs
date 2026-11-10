//! The protocol floor, by `hello` (design-audio-mute P14; R1 decided by the merge gate; the
//! cut-down replaces the binary marker scan with the coupled update's hello check). An engine
//! older than the sync handshake (a half-finished install: the app was updated, `recall.exe`
//! was not) records before any line can reach it and cannot retire a track. Its `hello` says no
//! `sync_hold`: it is restarted, and every later spawn of this controller run is given no input
//! and no picked device (game audio, the Windows default output and app tracks still record).
//! The user is told, with the reason, by a notice shown even with notices off, with a Reinstall
//! button in the app.

use std::path::Path;

use crate::engine_policy::EngineSpec;
use crate::record_audio_sources::{AudioSource, SOURCES_ENV};

/// The controller event that raises the notice (`record_inputs_held::notice_for`).
pub(crate) const ENGINE_OUTDATED: &str = "engine_outdated";
const GAME_PID_ENV: &str = "RECALL_AUDIO_GAME_PID";
/// The legacy switch that keeps an old engine's default microphone off.
const MIC_OFF_ENV: &str = "RECALL_AUDIO_MIC_OFF";

/// `list` without what an engine below the floor may not get: every input, and every source
/// with a picked device (a named endpoint or a saved id). The game stays when it is captured by
/// its process tree (`process_loopback`) or on the Windows default output; outputs on the
/// default and app tracks stay.
pub(crate) fn withhold_devices(list: &[AudioSource], process_loopback: bool) -> Vec<AudioSource> {
    let picked = |s: &AudioSource| !s.device.trim().is_empty() || !s.device_id.trim().is_empty();
    list.iter()
        .filter(|s| match s.kind.as_str() {
            "input" => false,
            "game" => process_loopback || !picked(s),
            "output" => !picked(s),
            _ => true,
        })
        .cloned()
        .collect()
}

/// [`withhold_devices`] on an engine list value (`RECALL_AUDIO_SOURCES`): `[]` when unreadable.
fn withhold_value(value: &str, process_loopback: bool) -> String {
    let list = crate::audio_intent::parse(value).unwrap_or_default();
    serde_json::to_string(&withhold_devices(&list, process_loopback)).unwrap_or_else(|_| "[]".into())
}

impl super::Shared {
    /// The spec a spawn of the engine at `path` gets: unchanged, until an engine's `hello` said no
    /// `sync_hold`; from then on its sources withheld as [`withhold_devices`] says, and the
    /// controller told ([`ENGINE_OUTDATED`]).
    pub(super) fn floor_spec(&self, path: &Path, spec: &EngineSpec) -> EngineSpec {
        let below = self.spawn_sources.forced_below();
        let process_loopback = spec.env.iter().any(|(k, v)| k == GAME_PID_ENV && v.is_some());
        self.spawn_sources.set_below_floor(below.then_some(process_loopback));
        if !below {
            return spec.clone();
        }
        crate::log!("supervisor: {} said no sync_hold (an older recorder); it records no input or picked device", path.display());
        let mut spec = spec.clone();
        for entry in spec.env.iter_mut().filter(|(k, _)| k == SOURCES_ENV) {
            entry.1 = Some(withhold_value(entry.1.as_deref().unwrap_or("[]"), process_loopback));
        }
        // An engine older than RECALL_AUDIO_SOURCES reads the legacy vars: no default mic either.
        spec.env.retain(|(k, _)| k != MIC_OFF_ENV);
        spec.env.push((MIC_OFF_ENV.to_owned(), Some("1".to_owned())));
        self.spawn_sources.rebake(&spec);
        self.tell_controller(serde_json::json!({ "type": ENGINE_OUTDATED }));
        spec
    }

    /// A `hello` without `sync_hold` (P14, the cut-down's mixed-install check): that engine
    /// recorded before any line reached it. Unless it was already given no device, every later
    /// spawn withholds them and this engine is restarted now, and the user is told.
    pub(super) fn on_hello_floor(&self, hello: &serde_json::Value) {
        if hello.get("sync_hold").and_then(serde_json::Value::as_bool) == Some(true) || self.spawn_sources.below_floor() {
            return;
        }
        crate::log!("supervisor: the engine's hello has no sync_hold; restarting it with no input or picked device");
        self.spawn_sources.force_below();
        self.tell_controller(serde_json::json!({ "type": ENGINE_OUTDATED }));
        let mut g = self.desired.lock().unwrap_or_else(|p| p.into_inner());
        if g.1.is_some() {
            g.0 += 1;
            g.2 = crate::record_dispatch::Sent::Restarting;
        }
    }

    /// An event for the controller's engine-event loop that no engine line carries.
    fn tell_controller(&self, ev: serde_json::Value) {
        if let Ok(g) = self.events.lock() {
            if let Some(tx) = g.as_ref() {
                let _ = tx.send(ev);
            }
        }
    }
}

impl super::Supervisor {
    /// The running (or next) engine is below the protocol floor: it was given no input or picked
    /// device and ignores the sync handshake's lines.
    pub fn engine_below_floor(&self) -> bool {
        self.shared.spawn_sources.below_floor()
    }
}

#[cfg(test)]
#[path = "supervisor_floor_tests.rs"]
mod tests;
