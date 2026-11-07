//! One WASAPI capture stream of a device or game track: open it (process loopback, or the
//! source's endpoint), and capture into the track's ring until it fails. All COM objects live
//! on the calling (capture) thread; `capture_loop` drives open and reopen.

use std::collections::VecDeque;
use std::time::Duration;

use wasapi::{Direction, SampleType, StreamMode, WaveFormat};

use super::apps::Leveller;
use super::capture_loop::{silence_to_clock, watch_for, Capture, Clock, Ended, Notices, Watch};
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
}

/// Open `target`'s process loopback, else the source's endpoint (a named one by its name
/// only), and start it.
pub(super) fn open_stream(direction: &Direction, device: &str, ring: &TrackRing, target: &Option<LoopbackTarget>) -> Result<Stream, String> {
    let format = WaveFormat::new(32, 32, &SampleType::Float, ring.rate as usize, ring.channels as usize, None);
    let mut watch = None;
    let (mut client, endpoint, buffer_duration_hns) = match target {
        Some(LoopbackTarget { pid, label }) => (
            wasapi::AudioClient::new_application_loopback_client(*pid, true).map_err(|e| format!("process loopback for PID {pid}: {e}"))?,
            label.clone(),
            // Process-loopback clients do not support get_device_period.
            // 20 ms is the wasapi example's documented safe event period.
            200_000,
        ),
        None => {
            let (dev, endpoint, on_default) = super::pick_device(direction, device, ring.input)?;
            if on_default {
                watch = watch_for(device, dev.get_id().ok());
            }
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
        Ok(capture) => Ok(Stream { client, event, capture, endpoint, device: target.is_none(), watch }),
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
        let mut last_sound = std::time::Instant::now();
        loop {
            if let Some(ended) = self.watch.as_mut().and_then(|w| w.check(notices)) {
                return ended;
            }
            if self.event.0.wait_for_event(WAIT_MS).is_ok() {
                if let Err(e) = self.capture.read_from_device_to_deque(&mut raw) {
                    return lost(format!("read failed: {e}"));
                }
                if raw.len() >= block {
                    let full = raw.len() - raw.len() % block;
                    let samples: Vec<f32> = raw.drain(..full).collect::<Vec<u8>>().chunks_exact(4).map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]])).collect();
                    ring.push_capture(&level.live(clock.elapsed(), &samples, channels), super::mic_muted());
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

    fn run(&mut self, ring: &TrackRing, level: &mut Leveller, clock: &dyn Clock, notices: &dyn Notices) -> Ended {
        let ended = self.capture(ring, level, clock, notices);
        let _ = self.client.stop_stream();
        ended
    }
}
