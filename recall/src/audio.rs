//! WASAPI capture for the replay buffer (feature `encode`).
//!
//! A track = one recorded source (`sources.rs`, the Audio page's strips): the game's process
//! tree, an output device by loopback, a microphone or line-in, or an app's process tree
//! (`apps.rs`). Each is titled with the source's name, scaled by its gain before anything is
//! stored, and the mic-mute hotkey silences every input source. A device that is gone falls
//! back to the Windows default and the status says so (`sources::opened`).
//!
//! Each track runs its own capture thread pushing interleaved f32 samples into a ring
//! holding the last `buffer_secs`. The encoder snapshots rings at save time and encodes AAC
//! per track — raw PCM in RAM keeps encoder state out of the ring (~23MB / 60s / stereo).

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use wasapi::{DeviceCollection, Direction, SampleType, StreamMode, WaveFormat};

mod apps;
pub mod sources;
mod voice;

use sources::{Kind, SourceSpec};

static MIC_MUTED: AtomicBool = AtomicBool::new(false);

/// Set whether newly captured input samples enter the replay buffer as silence.
pub fn set_mic_muted(muted: bool) {
    MIC_MUTED.store(muted, Ordering::Relaxed);
}

/// Toggle microphone capture muting and return the new state.
pub fn toggle_mic_mute() -> bool {
    MIC_MUTED.fetch_xor(true, Ordering::Relaxed) ^ true
}

/// Return whether newly captured input samples are muted.
pub fn mic_muted() -> bool {
    MIC_MUTED.load(Ordering::Relaxed)
}

/// A process tree captured by process loopback (include-tree mode).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LoopbackTarget {
    pub pid: u32,
    /// Shown as the track's "endpoint" in logs.
    pub label: String,
}

/// One track's PCM captured for a save: interleaved f32, front-padded to the
/// clip length. Owned and `Send`, so the saver thread encodes it off-ring.
pub struct AudioPcm {
    pub name: &'static str,
    /// The stream's `title` in the clip (the source's name).
    pub title: &'static str,
    /// Where the track came from (`edit_model::SOURCE_*` / `app:<exe>`), saved in the clip.
    pub source: &'static str,
    pub rate: u32,
    pub channels: u16,
    pub samples: Vec<f32>,
    /// The source's gain is above 0 dB: the save runs the peak limiter before the AAC
    /// encode, so a boosted source never hard-clips. Tracks at or below 0 dB stay as captured.
    pub boosted: bool,
}

impl AudioPcm {
    /// This track through the -1 dBFS look-ahead limiter (`export::mix_limit`).
    pub fn limited(&self) -> AudioPcm {
        AudioPcm {
            samples: crate::export::mix_limit::limit_interleaved(&self.samples, self.channels, self.rate),
            boosted: false,
            ..*self
        }
    }
}

/// Interleaved f32 ring + the format it was captured in.
pub struct TrackRing {
    /// The source's id: unique in a run (long-recording lanes, thread names).
    pub name: &'static str,
    /// The source's name: the clip's stream title.
    pub title: &'static str,
    /// Where the track comes from (`edit_model::SOURCE_*` / `app:<exe>`).
    pub source: &'static str,
    /// Linear gain applied to every captured sample.
    gain: f32,
    /// An input device: silenced by the mic-mute hotkey.
    input: bool,
    /// Resolved endpoint friendly name (set by the capture thread at init).
    endpoint: Mutex<String>,
    pub rate: u32,
    pub channels: u16,
    samples: Mutex<VecDeque<f32>>,
    capacity: usize,
}

/// Fixed 48 kHz stereo f32 for every track; WASAPI autoconverts from the endpoint's format.
const RATE: u32 = 48_000;
const CHANNELS: u16 = 2;

impl TrackRing {
    /// A ring for `spec` holding `buffer_secs` of 48 kHz stereo. Names are `&'static str`
    /// across the engine; a run holds at most `SOURCES_MAX` sources for its whole life, so
    /// leaking them is bounded.
    fn for_source(spec: &SourceSpec, source: &'static str, buffer_secs: u32, endpoint: String) -> Self {
        TrackRing {
            name: Box::leak(spec.id.trim().to_owned().into_boxed_str()),
            title: Box::leak(spec.name.trim().to_owned().into_boxed_str()),
            source,
            gain: spec.gain(),
            input: spec.kind() == Some(Kind::Input),
            endpoint: Mutex::new(endpoint),
            rate: RATE,
            channels: CHANNELS,
            samples: Mutex::new(VecDeque::new()),
            capacity: buffer_secs as usize * RATE as usize * CHANNELS as usize,
        }
    }

    pub fn endpoint(&self) -> String {
        self.endpoint.lock().map(|s| s.clone()).unwrap_or_default()
    }

    /// Snapshot the newest `secs` of audio, front-padded with silence to
    /// exactly `secs` so all tracks align at the clip start.
    ///
    /// The capture thread's `push_capture` shares this lock, so the critical
    /// section is only the ring's two contiguous halves `memcpy`d into pages
    /// that were allocated and faulted in before the lock was taken.
    pub fn snapshot(&self, secs: f64) -> Vec<f32> {
        let want = (secs * self.rate as f64) as usize * self.channels as usize;
        // Zeroed allocation doubles as the silence padding. Touch one sample per
        // page so the copy below never takes demand-zero page faults under the
        // lock (black_box keeps the store from being elided as redundant).
        let mut out = vec![0.0f32; want];
        const PAGE_SAMPLES: usize = 4096 / std::mem::size_of::<f32>();
        for i in (0..want).step_by(PAGE_SAMPLES) {
            out[i] = std::hint::black_box(0.0);
        }
        let buf = self.samples.lock().expect("ring lock");
        let have = buf.len().min(want);
        let skip = buf.len() - have;
        let (head, tail) = buf.as_slices();
        let dst = &mut out[want - have..];
        if skip < head.len() {
            let from_head = &head[skip..];
            dst[..from_head.len()].copy_from_slice(from_head);
            dst[from_head.len()..].copy_from_slice(tail);
        } else {
            dst.copy_from_slice(&tail[skip - head.len()..]);
        }
        drop(buf);
        out
    }

    /// Snapshot `secs` of audio together with the format needed to encode it,
    /// so the clip's audio is fixed at save-request time on the encoder thread.
    pub fn snapshot_pcm(&self, secs: f64) -> AudioPcm {
        AudioPcm {
            name: self.name,
            title: self.title,
            source: self.source,
            rate: self.rate,
            channels: self.channels,
            samples: self.snapshot(secs),
            boosted: self.gain > 1.0,
        }
    }

    /// Append captured samples, scaled by the source's gain. The mic-mute hotkey silences
    /// only input sources, and only samples entering the ring: replay history never changes.
    fn push_capture(&self, data: &[f32], mute_inputs: bool) {
        let silence = mute_inputs && self.input;
        let gained: Vec<f32>;
        let data = if self.gain != 1.0 && !silence {
            gained = data.iter().map(|s| s * self.gain).collect();
            &gained[..]
        } else {
            data
        };
        crate::longrec::tap::tap_audio(self.name, data, silence); // long-recording hook
        let mut buf = self.samples.lock().expect("ring lock");
        for &s in data {
            buf.push_back(if silence { 0.0 } else { s });
        }
        let over = buf.len().saturating_sub(self.capacity);
        if over > 0 {
            buf.drain(..over);
        }
    }
}

/// Resolve + start every source in list order. Failures are per-source (a broken input
/// must not kill game audio); returns whatever started, with a report line each.
pub fn start(buffer_secs: u32, mut report: impl FnMut(String)) -> Vec<Arc<TrackRing>> {
    report(format!(
        "audio endpoints -- render: {:?} | capture: {:?}",
        list_endpoints(&Direction::Render),
        list_endpoints(&Direction::Capture)
    ));
    let list = match sources::from_env() {
        Ok(list) => list,
        Err(e) => {
            report(format!("audio sources UNAVAILABLE: {e}"));
            return Vec::new();
        }
    };
    let game_pid = game_process_loopback_pid();
    let mut out = Vec::new();
    for spec in list.iter().filter(|s| s.kind() != Some(Kind::App)) {
        let target = match (spec.kind(), &game_pid) {
            (Some(Kind::Game), Ok(pid)) => pid.map(|pid| LoopbackTarget { pid, label: format!("process {pid} (include tree)") }),
            (Some(Kind::Game), Err(e)) => {
                report(format!("audio track '{}' UNAVAILABLE: {e}", spec.name));
                sources::note_failed(spec, e.clone());
                continue;
            }
            _ => None,
        };
        match start_track(spec, buffer_secs, target) {
            Ok(ring) => {
                report(format!("audio track '{}' up: {} ({} Hz, {} ch)", ring.title, ring.endpoint(), ring.rate, ring.channels));
                sources::note_live(spec, &ring);
                out.push(ring);
            }
            Err(e) => {
                report(format!("audio track '{}' UNAVAILABLE: {e}", spec.name));
                sources::note_failed(spec, e);
            }
        }
    }
    let app_rings = apps::start(&sources::app_sources(&list), buffer_secs, game_pid.clone().ok().flatten(), &mut report);
    for ring in &app_rings {
        if let Some(spec) = list.iter().find(|s| s.id.trim() == ring.name) {
            sources::note_live(spec, ring);
        }
    }
    out.extend(app_rings);
    sources::in_list_order(&list, out)
}

/// List every endpoint's friendly name for a direction (startup diagnostics /
/// morning remap reference).
pub fn list_endpoints(direction: &Direction) -> Vec<String> {
    let _ = wasapi::initialize_mta();
    let Ok(collection) = DeviceCollection::new(direction) else {
        return Vec::new();
    };
    let n = collection.get_nbr_devices().unwrap_or(0);
    (0..n)
        .filter_map(|i| collection.get_device_at_index(i).ok())
        .filter_map(|d| d.get_friendlyname().ok())
        .collect()
}

/// True when every whitespace-separated token of `want` appears in `name`
/// (both lowercased) — survives vendor naming like
/// "Speakers (2- USB Audio Device)" for "usb speakers".
fn name_matches(name: &str, want: &str) -> bool {
    let name = name.to_lowercase();
    want.split_whitespace()
        .all(|tok| name.contains(&tok.to_lowercase()))
}

/// Index of the endpoint `want` names: an exact (case-insensitive) friendly
/// name first, so "Microphone (2- USB)" never loses to a longer name that
/// shares its tokens; then the first token-AND match.
fn match_endpoint(names: &[String], want: &str) -> Option<usize> {
    let want = want.trim();
    if want.is_empty() {
        return None;
    }
    let exact = names.iter().position(|n| n.trim().to_lowercase() == want.to_lowercase());
    exact.or_else(|| names.iter().position(|n| name_matches(n, want)))
}

/// The endpoint a device source asks for: its device, else the Windows default. Under
/// `RECALL_AUDIO_EXACT` (tests) only a named endpoint may open — an input with no device
/// takes `RECALL_AUDIO_MIC`'s — so a test never records the person's real devices.
fn pick_device(direction: &Direction, device: &str, input: bool) -> Result<(wasapi::Device, String), String> {
    let collection = DeviceCollection::new(direction).map_err(|e| format!("enumerate: {e}"))?;
    let n = collection.get_nbr_devices().map_err(|e| e.to_string())?;
    let exact = std::env::var_os("RECALL_AUDIO_EXACT").is_some();
    let test_mic = (exact && input).then(|| std::env::var("RECALL_AUDIO_MIC").ok()).flatten();
    let want = Some(device.trim().to_owned()).filter(|w| !w.is_empty()).or(test_mic).filter(|w| !w.trim().is_empty());

    if let Some(want) = &want {
        let devices: Vec<wasapi::Device> = (0..n).filter_map(|i| collection.get_device_at_index(i).ok()).collect();
        let names: Vec<String> = devices.iter().map(|d| d.get_friendlyname().unwrap_or_default()).collect();
        if let Some(i) = match_endpoint(&names, want) {
            let name = names[i].clone();
            return Ok((devices.into_iter().nth(i).expect("index from names"), name));
        }
    }
    if exact {
        return Err(format!("no endpoint matches {want:?} (RECALL_AUDIO_EXACT)"));
    }
    let dev = wasapi::get_default_device(direction).map_err(|e| format!("default endpoint failed: {e}"))?;
    let name = dev.get_friendlyname().unwrap_or_else(|_| "(default)".into());
    Ok(match want {
        Some(want) => (dev, format!("{name} (Windows default; '{want}' not found)")),
        None => (dev, format!("{name} (Windows default)")),
    })
}

/// Resolve the opt-in process-loopback target for Game capture. A supplied
/// value deliberately never falls back to endpoint capture: that could record
/// the wrong audio source while appearing to work.
fn game_process_loopback_pid() -> Result<Option<u32>, String> {
    match std::env::var("RECALL_AUDIO_GAME_PID") {
        Ok(value) => parse_pid("RECALL_AUDIO_GAME_PID", &value).map(Some),
        Err(std::env::VarError::NotPresent) => Ok(None),
        Err(std::env::VarError::NotUnicode(_)) => {
            Err("RECALL_AUDIO_GAME_PID is not valid Unicode".into())
        }
    }
}

/// A positive process ID from an env value named `key`.
fn parse_pid(key: &str, value: &str) -> Result<u32, String> {
    match value.trim().parse::<u32>() {
        Ok(0) => Err(format!("{key} must be a positive process ID, got 0")),
        Ok(pid) => Ok(pid),
        Err(_) => Err(format!("{key} must be a positive process ID, got {value:?}")),
    }
}

/// The saved source tag of a device track: the game's process tree, an output it listens
/// to, or an input. The editor reads these (`edit_model::SOURCE_*`).
fn source_of(kind: Option<Kind>, process: bool) -> &'static str {
    use crate::edit_model::{SOURCE_DESKTOP, SOURCE_GAME_PROCESS, SOURCE_MIC};
    match kind {
        Some(Kind::Game) if process => SOURCE_GAME_PROCESS,
        Some(Kind::Input) => SOURCE_MIC,
        _ => SOURCE_DESKTOP,
    }
}

/// Spawn the capture thread for one device source. ALL WASAPI/COM objects are created
/// on the capture thread (they are !Send); init success/failure is reported
/// back over a channel before this returns. `target` selects process loopback
/// instead of the source's endpoint.
fn start_track(spec: &SourceSpec, buffer_secs: u32, target: Option<LoopbackTarget>) -> Result<Arc<TrackRing>, String> {
    let ring = Arc::new(TrackRing::for_source(spec, source_of(spec.kind(), target.is_some()), buffer_secs, String::new()));
    let direction = if spec.kind() == Some(Kind::Input) { Direction::Capture } else { Direction::Render };
    let device = spec.device.clone();
    let tring = ring.clone();
    let (init_tx, init_rx) = std::sync::mpsc::channel::<Result<(), String>>();
    std::thread::Builder::new()
        .name(format!("recall-audio-{}", ring.name))
        .spawn(move || capture_thread(direction, device, tring, init_tx, target))
        .map_err(|e| e.to_string())?;

    match init_rx.recv_timeout(std::time::Duration::from_secs(5)) {
        Ok(Ok(())) => Ok(ring),
        Ok(Err(e)) => Err(e),
        Err(_) => Err("audio init timed out".into()),
    }
}

fn capture_thread(
    direction: Direction,
    device: String,
    ring: Arc<TrackRing>,
    init_tx: std::sync::mpsc::Sender<Result<(), String>>,
    target: Option<LoopbackTarget>,
) {
    let init = || -> Result<_, String> {
        // Per-thread COM init; already-initialized is fine.
        let _ = wasapi::initialize_mta();
        let format = WaveFormat::new(32, 32, &SampleType::Float, ring.rate as usize, ring.channels as usize, None);
        let (mut client, endpoint, buffer_duration_hns) = match &target {
            Some(LoopbackTarget { pid, label }) => (
                wasapi::AudioClient::new_application_loopback_client(*pid, true)
                    .map_err(|e| format!("process loopback for PID {pid}: {e}"))?,
                label.clone(),
                // Process-loopback clients do not support get_device_period.
                // 20 ms is the wasapi example's documented safe event period.
                200_000,
            ),
            None => {
                let (device, endpoint) = pick_device(&direction, &device, ring.input)?;
                let client = device.get_iaudioclient().map_err(|e| e.to_string())?;
                let (_def_period, min_period) = client.get_device_period().map_err(|e| e.to_string())?;
                (client, endpoint, min_period)
            }
        };
        let mode = StreamMode::EventsShared { autoconvert: true, buffer_duration_hns };
        // Loopback = a RENDER endpoint's client initialized Capture-direction.
        client
            .initialize_client(&format, &Direction::Capture, &mode)
            .map_err(|e| format!("initialize: {e}"))?;
        let event = client.set_get_eventhandle().map_err(|e| e.to_string())?;
        let capture = client.get_audiocaptureclient().map_err(|e| e.to_string())?;
        client.start_stream().map_err(|e| e.to_string())?;
        if let Ok(mut g) = ring.endpoint.lock() {
            *g = endpoint;
        }
        Ok((client, event, capture))
    };

    let (_client, event, capture) = match init() {
        Ok(v) => {
            let _ = init_tx.send(Ok(()));
            v
        }
        Err(e) => {
            let _ = init_tx.send(Err(e));
            return;
        }
    };

    let block = ring.channels as usize * 4; // f32 frame size in bytes
    let mut raw: VecDeque<u8> = VecDeque::new();
    loop {
        if event.wait_for_event(2000).is_err() {
            continue; // silence timeout -- keep waiting
        }
        if capture.read_from_device_to_deque(&mut raw).is_err() {
            continue;
        }
        if raw.len() < block {
            continue;
        }
        let full = raw.len() - raw.len() % block;
        let bytes: Vec<u8> = raw.drain(..full).collect();
        let mut samples = Vec::with_capacity(bytes.len() / 4);
        for chunk in bytes.chunks_exact(4) {
            samples.push(f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]));
        }
        ring.push_capture(&samples, mic_muted());
    }
}

#[cfg(test)]
#[path = "audio_tests.rs"]
mod tests;
