//! One WASAPI capture stream of a device or game track: open it (process loopback, or the
//! source's endpoint), and capture into the track's ring until it fails. All COM objects live
//! on the calling (capture) thread; `capture_loop` drives open and reopen.

use std::collections::VecDeque;
use std::time::Duration;

use wasapi::{DeviceCollection, Direction, SampleType, StreamMode, WaveFormat};

use super::apps::Leveller;
#[path = "../communications_audio.rs"]
mod communications_audio;

use super::capture_loop::{silence_to_clock, watch_for, Capture, Clock, Ended, Notices, Watch};
use super::endpoints::{choose_endpoint_by, opened_own, Pick};
use super::rebind::{key_for, rebind_for, Key, Rebind, Run};
use super::{LoopbackTarget, TrackRing};

/// No packet for this long: the source is quiet (loopback sends nothing while nothing
/// plays, a closed game's process sends nothing), so its track gets silence up to the clock.
const IDLE: Duration = Duration::from_millis(200);
/// Event wait per turn: short, so a quiet source is filled promptly.
const WAIT_MS: u32 = 100;

/// The WASAPI buffer a device stream asks for: 100 ms, or the device's own period when it is
/// longer. The minimum period (about 3 ms, rounded up to one ~10 ms engine period) let a
/// short stall of this BelowNormal process overrun the buffer and drop samples (a crackle).
pub(super) fn device_buffer_hns(min_period: i64) -> i64 {
    min_period.max(1_000_000)
}

/// Join the calling capture thread to MMCSS's "Audio" task, so Windows schedules it ahead of
/// ordinary work (it stays on the cores the engine is pinned to). A refusal is logged, not fatal.
pub(super) fn join_mmcss_audio(track: &str) {
    let mut index = 0u32;
    // SAFETY: a static wide string and a valid out pointer; the task stays joined for the
    // thread's life (Windows reverts it when the thread ends).
    if let Err(e) = unsafe { windows::Win32::System::Threading::AvSetMmThreadCharacteristicsW(windows::core::w!("Audio"), &mut index) } {
        eprintln!("[recall] audio track '{track}': no MMCSS audio scheduling ({e})");
    }
}

/// wasapi's event handle, closed on drop: wasapi 0.19's `Handle` has no `Drop`, so every
/// reopen used to leak one event handle. Drop it after the client that signals it.
pub(super) struct Event(pub(super) wasapi::Handle);

// `Handle` is one `HANDLE` field (wasapi 0.19 api.rs): the same size means it sits at offset 0.
const _: () = assert!(std::mem::size_of::<wasapi::Handle>() == std::mem::size_of::<*mut std::ffi::c_void>());

impl Drop for Event {
    fn drop(&mut self) {
        // SAFETY: the size check above; the handle came from CreateEventA and is closed once.
        let raw: *mut std::ffi::c_void = unsafe { std::mem::transmute_copy(&self.0) };
        if !raw.is_null() {
            let _ = unsafe { windows::Win32::Foundation::CloseHandle(windows::Win32::Foundation::HANDLE(raw)) };
        }
    }
}

/// One open capture stream. Fields drop in order: the client before its event.
pub(super) struct Stream {
    client: wasapi::AudioClient,
    capture: wasapi::AudioCaptureClient,
    event: Event,
    endpoint: String,
    /// A device endpoint (not process loopback): an unplug shows as a padding error.
    device: bool,
    /// Set when a source with no device records the Windows default (`capture_loop::watch_for`).
    watch: Option<Watch>,
    /// The endpoint is the source's own saved device (`endpoints::opened_own`).
    own: bool,
    /// A named device: ends when a device notice changes its pick (`rebind::rebind_for`).
    rebind: Option<Rebind>,
    /// An input's unsure exact-name sibling (A4 ruling): held muted, with its endpoint id.
    unsure: Option<Option<String>>,
}

/// The endpoint `pick_device` opened.
struct Picked {
    dev: wasapi::Device,
    endpoint: String,
    /// The Windows default was opened (the stream watches it).
    on_default: bool,
    /// It is the saved device itself: the endpoint id it saved (`endpoints::opened_own`).
    own: bool,
    /// What a re-pick compares (`rebind::key_for`): the endpoint id, or the name with no saved id.
    key: Key,
    /// An unsure sibling (A4 ruling) and its endpoint id, offered as the source's own on unmute.
    unsure: Option<Option<String>>,
}

/// The re-pick key of the endpoint `name` that `device` (saved with `device_id`) opened, and
/// whether it is the saved device itself (`endpoints::opened_own`). With no saved id the
/// endpoint id is never read (R7-deep-audio-4): it cannot be own.
pub(super) fn own_decision(device: &str, device_id: &str, name: &str, id: impl FnOnce() -> Option<String>) -> (Key, bool) {
    let key = key_for(device_id, name, id);
    // Against the saved device only: the test mic stands in for the default.
    let own = matches!(&key, Key::Id(id) if opened_own(Some(id), device, device_id));
    (key, own)
}

/// Open what `choose_endpoint` picks for the saved `(device, device_id)`; an input with no
/// device takes `RECALL_AUDIO_MIC`'s under `RECALL_AUDIO_EXACT`.
fn pick_device(direction: &Direction, (device, device_id): (&str, &str), input: bool) -> Result<Picked, String> {
    let collection = DeviceCollection::new(direction).map_err(|e| format!("enumerate: {e}"))?;
    let n = collection.get_nbr_devices().map_err(|e| e.to_string())?;
    let exact = std::env::var_os("RECALL_AUDIO_EXACT").is_some();
    let test_mic = (exact && input).then(|| std::env::var("RECALL_AUDIO_MIC").ok()).flatten();
    let want = Some(device.trim().to_owned()).filter(|w| !w.is_empty()).or(test_mic).filter(|w| !w.trim().is_empty());
    let devices: Vec<wasapi::Device> = (0..n).filter_map(|i| collection.get_device_at_index(i).ok()).collect();
    let names: Vec<String> = devices.iter().map(|d| d.get_friendlyname().unwrap_or_default()).collect();
    // The saved id tells same-named endpoints apart (A4 amendment); the test mic has none.
    let want_id = if device.trim().is_empty() { "" } else { device_id };
    match choose_endpoint_by(&names, (want.as_deref(), want_id), (exact, input), |i| devices.get(i).and_then(|d| d.get_id().ok()))? {
        Pick::Named(i) => {
            let dev = devices.into_iter().nth(i).expect("index from names");
            let (key, own) = own_decision(device, device_id, &names[i], || dev.get_id().ok());
            Ok(Picked { dev, endpoint: names[i].clone(), on_default: false, own, key, unsure: None })
        }
        Pick::Unsure(i) => {
            let dev = devices.into_iter().nth(i).expect("index from names");
            // Its id is read to offer it as the source's own once a fresh Alt+M unmutes it.
            let id = dev.get_id().ok();
            let key = key_for(device_id, &names[i], || id.clone());
            Ok(Picked { dev, endpoint: names[i].clone(), on_default: false, own: false, key, unsure: Some(id) })
        }
        Pick::Default => {
            let dev = wasapi::get_default_device(direction).map_err(|e| format!("default endpoint failed: {e}"))?;
            let name = dev.get_friendlyname().unwrap_or_else(|_| "(default)".into());
            Ok(Picked { dev, endpoint: format!("{name} (Windows default)"), on_default: true, own: false, key: Key::Name(name), unsure: None })
        }
    }
}

/// The re-pick of a named stream: what `pick_device` opens now (None: no single endpoint
/// matches, A4). It runs on a probe thread (`rebind::Run::Thread`), which joins COM itself.
fn repick(direction: Direction, (device, device_id): (&str, &str), input: bool) -> super::rebind::Probe {
    let (saved, saved_id) = (device.to_owned(), device_id.to_owned());
    std::sync::Arc::new(move || {
        let joined = wasapi::initialize_mta().is_ok();
        let key = pick_device(&direction, (&saved, &saved_id), input).ok().filter(|p| !p.on_default).map(|p| p.key);
        if joined {
            wasapi::deinitialize();
        }
        key
    })
}

/// Open `target`'s process loopback, else the source's endpoint (a named one by its name
/// only), and start it.
pub(super) fn open_stream(direction: &Direction, (device, device_id): (&str, &str), ring: &TrackRing, target: &Option<LoopbackTarget>, communications: bool) -> Result<Stream, String> {
    let format = WaveFormat::new(32, 32, &SampleType::Float, ring.rate as usize, ring.channels as usize, None);
    let (mut watch, mut own, mut rebind, mut unsure) = (None, false, None, None);
    let (mut client, endpoint, buffer_duration_hns) = match target {
        Some(LoopbackTarget { pid, label }) => (
            wasapi::AudioClient::new_application_loopback_client(*pid, true).map_err(|e| format!("process loopback for PID {pid}: {e}"))?,
            label.clone(),
            // Process-loopback clients do not support get_device_period.
            // 20 ms is the wasapi example's documented safe event period.
            200_000,
        ),
        None => {
            let Picked { dev, endpoint, on_default, own: is_own, key, unsure: held } = if communications {
                let dev = communications_audio::distinct_device().ok_or("No separate Windows communications output")?;
                let endpoint = dev.get_friendlyname().unwrap_or_else(|_| "Windows communications output".into());
                Picked { dev, key: Key::Name(endpoint.clone()), endpoint, on_default: true, own: false, unsure: None }
            } else {
                pick_device(direction, (device, device_id), ring.input)?
            };
            unsure = held;
            if on_default {
                watch = watch_for(device, dev.get_id().ok());
            }
            own = is_own;
            // A named device (R7-deep-audio-1): on a device notice, pick again (on its own thread)
            // and end this stream when the pick is no longer this endpoint.
            rebind = rebind_for(device, key, repick(*direction, (device, device_id), ring.input), (Run::Thread, ring.live.repick_slot.clone()));
            let client = dev.get_iaudioclient().map_err(|e| e.to_string())?;
            let (_def_period, min_period) = client.get_device_period().map_err(|e| e.to_string())?;
            (client, endpoint, device_buffer_hns(min_period))
        }
    };
    let mode = StreamMode::EventsShared { autoconvert: true, buffer_duration_hns };
    // Loopback = a RENDER endpoint's client initialized Capture-direction.
    client.initialize_client(&format, &Direction::Capture, &mode).map_err(|e| format!("initialize: {e}"))?;
    let event = Event(client.set_get_eventhandle().map_err(|e| e.to_string())?);
    let started = client.get_audiocaptureclient().and_then(|capture| client.start_stream().map(|()| capture));
    match started {
        Ok(capture) => Ok(Stream { client, event, capture, endpoint, device: target.is_none(), watch, own, rebind, unsure }),
        Err(e) => {
            // The client still holds the event: release it first, as a Stream's drop does.
            drop(client);
            drop(event);
            Err(e.to_string())
        }
    }
}

impl Stream {
    /// Capture until the stream fails. Every packet goes through the track's `Leveller`, so
    /// the ring stays level with the wall clock: a save cuts every track on the same clock
    /// (`encoder::handoff`), quiet stretches included.
    fn capture(&mut self, ring: &TrackRing, level: &mut Leveller, clock: &dyn Clock, notices: &dyn Notices) -> Ended {
        let lost = |why: String| Ended { why, at_once: false };
        let block = ring.channels as usize * 4; // f32 frame size in bytes
        let channels = ring.channels as usize;
        let mut raw: VecDeque<u8> = VecDeque::new();
        // Kept across packets: no allocation per packet once they fit (gate review P2-c).
        let (mut samples, mut leveled) = (Vec::new(), Vec::new());
        let mut last_sound = std::time::Instant::now();
        loop {
            if let Some(ended) = self.watch.as_mut().and_then(|w| w.check(notices)) {
                return ended;
            }
            if let Some(ended) = self.rebind.as_mut().and_then(|r| r.check(notices)) {
                return ended;
            }
            // The controller saved another id for this source (P1-B): reopen on it.
            if ring.live.take_repick() {
                return Ended { why: "the saved device id changed".to_owned(), at_once: true };
            }
            if self.event.0.wait_for_event(WAIT_MS).is_ok() {
                if let Err(e) = self.capture.read_from_device_to_deque(&mut raw) {
                    return lost(format!("read failed: {e}"));
                }
                if raw.len() >= block {
                    super::packet::decode(&mut raw, block, &mut samples);
                    level.live_into(clock.elapsed(), &samples, channels, &mut leveled);
                    // Alt+M as it is now, for every packet (an early press applies at once).
                    ring.push_live(&leveled, super::MuteNow::read());
                    last_sound = std::time::Instant::now();
                    continue;
                }
            } else if self.device {
                // A device that was unplugged or invalidated answers this with an error.
                if let Err(e) = self.client.get_current_padding() {
                    return lost(format!("device lost: {e}"));
                }
            }
            if last_sound.elapsed() >= IDLE {
                silence_to_clock(ring, level, clock);
            }
        }
    }
}

impl Capture for Stream {
    fn endpoint(&self) -> String {
        self.endpoint.clone()
    }

    fn own(&self) -> bool {
        self.own
    }

    fn unsure(&self) -> Option<Option<String>> {
        self.unsure.clone()
    }

    fn run(&mut self, ring: &TrackRing, level: &mut Leveller, clock: &dyn Clock, notices: &dyn Notices) -> Ended {
        let ended = self.capture(ring, level, clock, notices);
        let _ = self.client.stop_stream();
        ended
    }
}

#[cfg(test)]
#[path = "stream_own_tests.rs"]
mod own_tests;
