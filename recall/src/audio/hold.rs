//! An input opened on an unsure exact-name sibling (A4 ruling, merge gate p9: two or more
//! endpoints share the saved name and no saved id picks one) records silence whatever Alt+M
//! says, until the first press after it opened that leaves the mic unmuted. Only then is the
//! endpoint's id offered as the source's own (`adopted`, `status.audio[].adopt_id`): the
//! controller saves it and sends it back live (`set_source device_id`), and the track reopens on
//! it as its own device.
//!
//! Never for a line-in (`SourceSpec::mic_mute = false`, P1-b): an adopted id would make the
//! sibling its own device, which skips Alt+M from then on, and with two identical dongles that
//! may be the microphone. Its stream stays held and the status says to pick the device on the
//! Audio page, where the pick goes through the identical-name confirm and the mic re-arm.
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
    /// A line-in's unmuting press was refused (the notice is shown once).
    refused: bool,
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
    /// Silence whatever Alt+M says: the controller's `sync` was not read yet (the sync handshake).
    startup: AtomicBool,
    /// The source is no longer recorded as this track (`retire`): held for good, no stream.
    retired: AtomicBool,
}

/// What a packet does with the hold.
#[derive(Debug, PartialEq)]
pub(super) enum Check {
    Free,
    Held,
    /// Released by this packet: show this endpoint again.
    Released(String),
    /// A line-in's unmuting press, refused: it stays held; show the pick notice for this endpoint.
    Refused(String),
}

impl Live {
    pub(super) fn with_saved_id(id: &str) -> Self {
        Live { saved_id: Mutex::new(id.trim().to_owned()), ..Live::default() }
    }

    /// Hold the stream that just opened `endpoint` (its id `id`, when read).
    pub(super) fn start_hold(&self, endpoint: &str, id: Option<String>) {
        *self.hold.lock().unwrap_or_else(|p| p.into_inner()) = Some(Held { endpoint: endpoint.to_owned(), id, since: None, refused: false });
        self.held.store(true, Ordering::SeqCst);
    }

    /// The held stream ended: the next one starts free.
    pub(super) fn end_hold(&self) {
        self.held.store(false, Ordering::SeqCst);
        self.hold.lock().unwrap_or_else(|p| p.into_inner()).take();
    }

    /// Whether this packet enters as silence. The first packet notes the presses so far; a
    /// later packet that sees more presses and the mic unmuted releases the hold, except on a
    /// `line_in` (see the module notes), which stays held.
    pub(super) fn check(&self, now: MuteNow, line_in: bool) -> Check {
        if !self.held.load(Ordering::SeqCst) {
            return Check::Free;
        }
        let mut hold = self.hold.lock().unwrap_or_else(|p| p.into_inner());
        let Some(held) = hold.as_mut() else { return Check::Free };
        let since = *held.since.get_or_insert(now.presses);
        if now.presses == since || now.muted {
            return Check::Held;
        }
        if line_in {
            let first = !std::mem::replace(&mut held.refused, true);
            return if first { Check::Refused(held.endpoint.clone()) } else { Check::Held };
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

    /// End the open stream at its next packet turn (it reopens only when not held).
    pub(super) fn request_repick(&self) {
        self.repick.store(true, Ordering::SeqCst);
    }

    /// The saved id changed since the stream opened (taken once).
    pub(super) fn take_repick(&self) -> bool {
        self.repick.swap(false, Ordering::SeqCst)
    }
}

impl TrackRing {
    /// Append a live packet: Alt+M as it is right now (`now`), and silence while the stream is
    /// held (see the module notes).
    #[cfg(test)]
    pub(crate) fn push_live(&self, data: &[f32], now: MuteNow) {
        self.push_live_as(data, now, self.live.startup.load(Ordering::SeqCst));
    }

    /// The capture thread's packet (`stream.rs`): the startup hold is loaded BEFORE Alt+M is
    /// read (`read`, merge-gate F1). Seen released means the release came after every queued
    /// line was applied, so the mute read next sees them; read the other way round, a release
    /// between the two reads stored a stale "unmuted" packet live.
    pub(crate) fn push_live_read(&self, data: &[f32], read: impl FnOnce() -> MuteNow) {
        let startup = self.live.startup.load(Ordering::SeqCst); // the safe state: hold first
        self.push_live_as(data, read(), startup);
    }

    fn push_live_as(&self, data: &[f32], now: MuteNow, startup: bool) {
        // A line-in never takes a sibling's id (P1-b): read per packet, as the flag changes live.
        let line_in = !self.mic_mute.load(std::sync::atomic::Ordering::Relaxed);
        let held = match self.live.check(now, line_in) {
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
            Check::Refused(endpoint) => {
                eprintln!("[recall] audio track '{}': {endpoint} is a line-in on an unsure device; it stays muted until picked on the Audio page", self.name);
                if let Ok(mut e) = self.endpoint.lock() {
                    *e = super::endpoints::line_in_pick_notice(&endpoint);
                }
                true
            }
        };
        self.push_samples(data, now.muted, held || startup);
    }

    /// Held until the controller's `sync` (the sync handshake): this input stores silence
    /// until [`Self::release_startup_hold`], whatever Alt+M or its flag says.
    pub(crate) fn hold_startup(&self) {
        self.live.startup.store(true, Ordering::SeqCst);
    }

    /// Every line queued before the start was applied: the input records as its flags say.
    /// Never a retired track ([`Self::retire`]).
    pub(crate) fn release_startup_hold(&self) {
        if !self.live.retired.load(Ordering::SeqCst) {
            self.live.startup.store(false, Ordering::SeqCst);
        }
    }

    /// The source was removed, disabled or re-picked under another name (`set_source retire`,
    /// design-audio-mute P4): its next packet is silence (the startup hold, loaded before each
    /// packet), its open stream ends at the next packet turn (`take_repick`), and no stream
    /// opens again while it is held, which is for good.
    pub(crate) fn retire(&self) {
        self.live.retired.store(true, Ordering::SeqCst);
        self.live.startup.store(true, Ordering::SeqCst);
        self.live.request_repick();
    }

    pub(crate) fn retired(&self) -> bool {
        self.live.retired.load(Ordering::SeqCst)
    }

    /// Alt+M silences this track right now (the engine's truth for the meter's "Muted").
    pub(crate) fn mutes_now(&self) -> bool {
        super::mic_muted() && self.mutes()
    }

    /// Whether this track stores silence whatever Alt+M says right now: an unsure stream held
    /// (a line-in until picked included) or the startup hold. Reported in `status.audio[].held`.
    pub(crate) fn held_now(&self) -> bool {
        self.live.held.load(Ordering::SeqCst) || self.startup_held()
    }

    /// Tests: the endpoint id the next open picks with.
    #[cfg(test)]
    pub(crate) fn saved_id_for_test(&self) -> String {
        self.live.saved_id()
    }

    /// Tests: the source's "Alt+M mutes it" flag as set (live) now.
    #[cfg(test)]
    pub(crate) fn mic_mute_flag(&self) -> bool {
        self.mic_mute.load(std::sync::atomic::Ordering::SeqCst)
    }

    pub(crate) fn startup_held(&self) -> bool {
        self.live.startup.load(Ordering::SeqCst)
    }
}

/// Wait for a capture thread's init answer, at most `timeout`. A thread that does not answer in
/// time keeps running unseen by the controller (no `set_source` reaches an unlisted track):
/// it is retired, so it stores silence and opens nothing (design-audio-mute P18).
pub(super) fn await_init(ring: &TrackRing, rx: &std::sync::mpsc::Receiver<Result<(), String>>, timeout: std::time::Duration) -> Result<(), String> {
    match rx.recv_timeout(timeout) {
        Ok(answer) => answer,
        Err(_) => {
            ring.retire(); // the safe state for a ring nobody lists
            Err("audio init timed out".into())
        }
    }
}

/// Whether a track starts held (the sync handshake): every device track under a controller
/// (`hold`); a process-loopback track never, as no line can change what it captures.
pub(crate) fn starts_held(hold: bool, process_loopback: bool) -> bool {
    hold && !process_loopback
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
