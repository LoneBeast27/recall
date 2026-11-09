//! An input opened on an unsure exact-name sibling (A4 ruling, merge gate p9: two or more
//! endpoints share the saved name and no saved id picks one) records silence whatever Alt+M
//! says, until the first press after it opened that leaves the mic unmuted. Only then is the
//! endpoint's id offered as the source's own (`adopted`, `status.audio[].adopt_id`): the
//! controller saves it and sends it back live (`set_source device_id`), and the track reopens on
//! it as its own device.
//!
//! Also here: the source's saved id as the engine knows it now (changed live, then the stream
//! reopens on it), and the re-pick slot (at most one re-pick thread per track).

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use super::mute::MuteNow;
use super::TrackRing;

/// One held stream: the endpoint shown once released, its id, and the presses at its first packet.
struct Held {
    endpoint: String,
    id: Option<String>,
    since: Option<u64>,
}

/// The per-track state that changes while the engine runs, apart from gain, name and flags.
#[derive(Default)]
pub(crate) struct Live {
    /// Fast path: no stream is held (no lock per packet).
    held: AtomicBool,
    hold: Mutex<Option<Held>>,
    /// The endpoint id a released hold offers as the source's own.
    adopted: Mutex<Option<String>>,
    /// The saved endpoint id the next open picks with ("" = none).
    saved_id: Mutex<String>,
    /// The saved id changed live: the open stream ends and reopens on it.
    repick: AtomicBool,
    /// A re-pick thread of this track runs (`rebind`): at most one at a time.
    pub(super) repick_slot: Arc<AtomicBool>,
}

/// What a packet does with the hold.
#[derive(Debug, PartialEq)]
pub(super) enum Check {
    Free,
    Held,
    /// Released by this packet: show this endpoint again.
    Released(String),
}

impl Live {
    pub(super) fn with_saved_id(id: &str) -> Self {
        Live { saved_id: Mutex::new(id.trim().to_owned()), ..Live::default() }
    }

    /// Hold the stream that just opened `endpoint` (its id `id`, when read).
    pub(super) fn start_hold(&self, endpoint: &str, id: Option<String>) {
        *self.hold.lock().unwrap_or_else(|p| p.into_inner()) = Some(Held { endpoint: endpoint.to_owned(), id, since: None });
        self.held.store(true, Ordering::SeqCst);
    }

    /// The held stream ended: the next one starts free.
    pub(super) fn end_hold(&self) {
        self.held.store(false, Ordering::SeqCst);
        self.hold.lock().unwrap_or_else(|p| p.into_inner()).take();
    }

    /// Whether this packet enters as silence. The first packet notes the presses so far; a
    /// later packet that sees more presses and the mic unmuted releases the hold.
    pub(super) fn check(&self, now: MuteNow) -> Check {
        if !self.held.load(Ordering::SeqCst) {
            return Check::Free;
        }
        let mut hold = self.hold.lock().unwrap_or_else(|p| p.into_inner());
        let Some(held) = hold.as_mut() else { return Check::Free };
        let since = *held.since.get_or_insert(now.presses);
        if now.presses == since || now.muted {
            return Check::Held;
        }
        let held = hold.take().expect("checked above");
        self.held.store(false, Ordering::SeqCst);
        if let Some(id) = held.id.filter(|id| super::endpoints::id_ok(id)) {
            *self.adopted.lock().unwrap_or_else(|p| p.into_inner()) = Some(id.trim().to_owned());
        }
        Check::Released(held.endpoint)
    }

    /// The endpoint id a released hold offers as the source's own.
    pub(crate) fn adopted(&self) -> Option<String> {
        self.adopted.lock().unwrap_or_else(|p| p.into_inner()).clone()
    }

    pub(crate) fn saved_id(&self) -> String {
        self.saved_id.lock().unwrap_or_else(|p| p.into_inner()).clone()
    }

    /// A new saved id from the controller: the stream reopens on it when it differs.
    pub(crate) fn set_saved_id(&self, id: &str) {
        let mut saved = self.saved_id.lock().unwrap_or_else(|p| p.into_inner());
        if saved.as_str() != id.trim() {
            *saved = id.trim().to_owned();
            self.repick.store(true, Ordering::SeqCst);
        }
    }

    /// The saved id changed since the stream opened (taken once).
    pub(super) fn take_repick(&self) -> bool {
        self.repick.swap(false, Ordering::SeqCst)
    }
}

impl TrackRing {
    /// Append a live packet: Alt+M as it is right now (`now`), and silence while the stream is
    /// held (see the module notes).
    pub(crate) fn push_live(&self, data: &[f32], now: MuteNow) {
        let held = match self.live.check(now) {
            Check::Free => false,
            Check::Held => true,
            Check::Released(endpoint) => {
                eprintln!("[recall] audio track '{}': {endpoint} unmuted by Alt+M; its id is saved as the source's own", self.name);
                // The controller saves it as the source's own (`record_audio_live`).
                if let Some(device_id) = self.live.adopted() {
                    crate::protocol::emit(&crate::protocol::Outbound::SourceAdopted { id: self.name.to_owned(), device_id });
                }
                if let Ok(mut e) = self.endpoint.lock() {
                    *e = endpoint;
                }
                false
            }
        };
        self.push_samples(data, now.muted, held);
    }
}

impl TrackRing {
    /// A 48 kHz stereo input ring on the Windows default, fed by the test (no WASAPI).
    #[cfg(test)]
    pub(crate) fn input_for_test(name: &'static str) -> Self {
        let spec = super::SourceSpec { communications: false, device_id: String::new(), id: name.into(), kind: "input".into(), device: String::new(), exe: String::new(), name: name.into(), gain_db: 0.0, mic_mute: None };
        Self::for_source(&spec, "mic", 2, String::new())
    }
}

#[cfg(test)]
#[path = "hold_tests.rs"]
mod tests;
