//! One WASAPI capture stream of a device or game track: open it (process loopback, or the
//! source's endpoint), and capture into the track's ring until it fails. All COM objects live
//! on the calling (capture) thread; `capture_loop` drives open and reopen.

use std::collections::VecDeque;
use std::time::Duration;

use wasapi::{Direction, SampleType, StreamMode, WaveFormat};

use super::apps::Leveller;
use super::capture_loop::{silence_to_clock, Capture, Clock, Ended};
use super::{LoopbackTarget, TrackRing};

/// No packet for this long: the source is quiet (loopback sends nothing while nothing
/// plays, a closed game's process sends nothing), so its track gets silence up to the clock.
const IDLE: Duration = Duration::from_millis(200);
/// Event wait per turn: short, so a quiet source is filled promptly.
const WAIT_MS: u32 = 100;
/// How often a Windows-default source checks whether the default moved to another device.
const DEFAULT_CHECK: Duration = Duration::from_secs(2);

/// A source recording the Windows default: notices when the default becomes another device.
pub(super) struct DefaultWatch {
    id: String,
    next: Duration,
}

impl DefaultWatch {
    /// Watching the default endpoint `id`, opened at `now` on the track's clock.
    pub(super) fn new(id: String, now: Duration) -> Self {
        DefaultWatch { id, next: now + DEFAULT_CHECK }
    }

    /// Every `DEFAULT_CHECK`: true when `current` (the default endpoint's id now) names another
    /// device. No default at all is not a move (the stream reports the lost device itself).
    pub(super) fn moved(&mut self, now: Duration, current: impl FnOnce() -> Option<String>) -> bool {
        if now < self.next {
            return false;
        }
        self.next = now + DEFAULT_CHECK;
        current().is_some_and(|id| id != self.id)
    }
}

/// One open capture stream.
pub(super) struct Stream {
    client: wasapi::AudioClient,
    event: wasapi::Handle,
    capture: wasapi::AudioCaptureClient,
    endpoint: String,
    /// A device endpoint (not process loopback): an unplug shows as a padding error.
    device: bool,
    direction: Direction,
    /// Set when this stream records the Windows default for a source with no device.
    watch: Option<DefaultWatch>,
}

fn default_id(direction: &Direction) -> Option<String> {
    wasapi::get_default_device(direction).ok()?.get_id().ok()
}

/// Open `target`'s process loopback, else the source's endpoint, and start it. `now` is the
/// track's clock (the default watch starts from it).
pub(super) fn open_stream(
    direction: &Direction,
    device: &str,
    ring: &TrackRing,
    target: &Option<LoopbackTarget>,
    reopen: bool,
    now: Duration,
) -> Result<Stream, String> {
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
            let (dev, endpoint) = super::pick_device(direction, device, ring.input, reopen)?;
            if device.trim().is_empty() && endpoint.ends_with("(Windows default)") {
                watch = dev.get_id().ok().map(|id| DefaultWatch::new(id, now));
            }
            let client = dev.get_iaudioclient().map_err(|e| e.to_string())?;
            let (_def_period, min_period) = client.get_device_period().map_err(|e| e.to_string())?;
            (client, endpoint, min_period)
        }
    };
    let mode = StreamMode::EventsShared { autoconvert: true, buffer_duration_hns };
    // Loopback = a RENDER endpoint's client initialized Capture-direction.
    client.initialize_client(&format, &Direction::Capture, &mode).map_err(|e| format!("initialize: {e}"))?;
    let event = client.set_get_eventhandle().map_err(|e| e.to_string())?;
    let capture = client.get_audiocaptureclient().map_err(|e| e.to_string())?;
    client.start_stream().map_err(|e| e.to_string())?;
    Ok(Stream { client, event, capture, endpoint, device: target.is_none(), direction: *direction, watch })
}

impl Stream {
    /// Capture until the stream fails. Every packet goes through the track's `Leveller`, so
    /// the ring stays level with the wall clock: a save cuts every track on the same clock
    /// (`encoder::handoff`), quiet stretches included.
    fn capture(&mut self, ring: &TrackRing, level: &mut Leveller, clock: &dyn Clock) -> Ended {
        let lost = |why: String| Ended { why, at_once: false };
        let block = ring.channels as usize * 4; // f32 frame size in bytes
        let channels = ring.channels as usize;
        let mut raw: VecDeque<u8> = VecDeque::new();
        let mut last_sound = std::time::Instant::now();
        loop {
            if let Some(watch) = &mut self.watch {
                if watch.moved(clock.elapsed(), || default_id(&self.direction)) {
                    return Ended { why: "the Windows default moved to another device".into(), at_once: true };
                }
            }
            if self.event.wait_for_event(WAIT_MS).is_ok() {
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

    fn run(&mut self, ring: &TrackRing, level: &mut Leveller, clock: &dyn Clock) -> Ended {
        let ended = self.capture(ring, level, clock);
        let _ = self.client.stop_stream();
        ended
    }
}
