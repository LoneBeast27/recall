//! WASAPI capture for the replay buffer (feature `encode`).
//!
//! A track = one recorded source (`sources.rs`, the Audio page's strips): the game's process
//! tree, an output device by loopback, a microphone or line-in, or an app's process tree
//! (`apps.rs`). Each is titled with the source's name, scaled by its gain before anything is
//! stored, and the mic-mute hotkey silences every input source. A device that is gone falls
//! back to the Windows default and the status says so (`sources::opened`).
//!
//! Each track runs its own capture thread pushing interleaved f32 samples into a ring
//! holding the last `buffer_secs`. Every track is kept level with the wall clock
//! (`apps::Leveller`): silence while its source sends nothing (loopback of a quiet output,
//! a closed game), and a lost device is reopened every `REOPEN_EVERY` meanwhile. Gain and
//! name change live (`sources::set_live`), so the Audio page never restarts the engine. The encoder snapshots rings at save time and encodes AAC
//! per track — raw PCM in RAM keeps encoder state out of the ring (~23MB / 60s / stereo).

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
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
    /// The source's name: the clip's stream title. A rename on the Audio page replaces it
    /// live (`set_title`), so it is read at save time.
    title: Mutex<&'static str>,
    /// Where the track comes from (`edit_model::SOURCE_*` / `app:<exe>`).
    pub source: &'static str,
    /// Linear gain applied to every captured sample, as `f32` bits: the Audio page's fader
    /// changes it live (`set_gain_db`), with no engine restart.
    gain: AtomicU32,
    /// The gain has been above 0 dB at some point: samples in the ring may pass full scale,
    /// so saves run the limiter until the engine restarts.
    boosted: AtomicBool,
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
            title: Mutex::new(Box::leak(spec.name.trim().to_owned().into_boxed_str())),
            source,
            gain: AtomicU32::new(spec.gain().to_bits()),
            boosted: AtomicBool::new(spec.gain() > 1.0),
            input: spec.kind() == Some(Kind::Input),
            endpoint: Mutex::new(endpoint),
            rate: RATE,
            channels: CHANNELS,
            samples: Mutex::new(VecDeque::new()),
            capacity: buffer_secs as usize * RATE as usize * CHANNELS as usize,
        }
    }

    /// The source's gain is, or has been, above 0 dB: every output of this track goes through
    /// the peak limiter (it leaves samples under the ceiling as they are).
    pub fn boosted(&self) -> bool {
        self.boosted.load(Ordering::Relaxed)
    }

    fn gain(&self) -> f32 {
        f32::from_bits(self.gain.load(Ordering::Relaxed))
    }

    /// New samples are scaled by `gain_db` (clamped like `SourceSpec::gain`); the replay
    /// history keeps the gain it was captured with.
    pub fn set_gain_db(&self, gain_db: f32) {
        let gain = sources::gain_of(gain_db);
        self.gain.store(gain.to_bits(), Ordering::Relaxed);
        if gain > 1.0 {
            self.boosted.store(true, Ordering::Relaxed);
        }
    }

    /// The clip's stream title from now on.
    pub fn title(&self) -> &'static str {
        self.title.lock().map(|t| *t).unwrap_or("")
    }

    /// Rename the track for the saves that follow. Names are leaked like the first one; a
    /// rename is a rare, hand-made change, so this stays small.
    pub fn set_title(&self, name: &str) {
        let name = name.trim();
        if name.is_empty() || name == self.title() {
            return;
        }
        if let Ok(mut t) = self.title.lock() {
            *t = Box::leak(name.to_owned().into_boxed_str());
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
    #[cfg(test)]
    pub fn snapshot(&self, secs: f64) -> Vec<f32> {
        self.snapshot_ending(secs, 0.0)
    }

    /// As [`Self::snapshot`], for the `secs` that end `tail_secs` before the
    /// newest sample: a save whose video ends before now (the game closed)
    /// leaves out the audio recorded after its last frame.
    pub fn snapshot_ending(&self, secs: f64, tail_secs: f64) -> Vec<f32> {
        let ch = self.channels as usize;
        let want = (secs * self.rate as f64) as usize * ch;
        let tail = (tail_secs.max(0.0) * self.rate as f64).round() as usize * ch;
        // Zeroed allocation doubles as the silence padding. Touch one sample per
        // page so the copy below never takes demand-zero page faults under the
        // lock (black_box keeps the store from being elided as redundant).
        let mut out = vec![0.0f32; want];
        const PAGE_SAMPLES: usize = 4096 / std::mem::size_of::<f32>();
        for i in (0..want).step_by(PAGE_SAMPLES) {
            out[i] = std::hint::black_box(0.0);
        }
        let buf = self.samples.lock().expect("ring lock");
        // Samples [skip, end) of the ring, end = newest minus the tail.
        let end = buf.len().saturating_sub(tail);
        let have = end.min(want);
        let skip = end - have;
        let (head, back) = buf.as_slices();
        let dst = &mut out[want - have..];
        // The part of [skip, end) in `head`, then the part in `back`.
        let h0 = skip.min(head.len());
        let h1 = end.min(head.len());
        let from_head = &head[h0..h1];
        dst[..from_head.len()].copy_from_slice(from_head);
        let b0 = skip.saturating_sub(head.len());
        let b1 = end.saturating_sub(head.len());
        dst[from_head.len()..].copy_from_slice(&back[b0..b1]);
        drop(buf);
        out
    }

    /// Snapshot `secs` of audio together with the format needed to encode it,
    /// so the clip's audio is fixed at save-request time on the encoder thread.
    #[cfg(test)]
    pub fn snapshot_pcm(&self, secs: f64) -> AudioPcm {
        self.snapshot_pcm_ending(secs, 0.0)
    }

    /// [`Self::snapshot_ending`] with its format (the encoder's `av_cut` sets the tail).
    pub fn snapshot_pcm_ending(&self, secs: f64, tail_secs: f64) -> AudioPcm {
        AudioPcm {
            name: self.name,
            title: self.title(),
            source: self.source,
            rate: self.rate,
            channels: self.channels,
            samples: self.snapshot_ending(secs, tail_secs),
            boosted: self.boosted(),
        }
    }

    /// Append captured samples, scaled by the source's gain. The mic-mute hotkey silences
    /// only input sources, and only samples entering the ring: replay history never changes.
    fn push_capture(&self, data: &[f32], mute_inputs: bool) {
        let silence = mute_inputs && self.input;
        let gain = self.gain();
        let gained: Vec<f32>;
        let data = if gain != 1.0 && !silence {
            gained = data.iter().map(|s| s * gain).collect();
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
                report(format!("audio track '{}' up: {} ({} Hz, {} ch)", ring.title(), ring.endpoint(), ring.rate, ring.channels));
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

/// How long a track waits between attempts to reopen a device or process it lost.
const REOPEN_EVERY: std::time::Duration = std::time::Duration::from_secs(2);
/// No packet for this long: the source is quiet (loopback sends nothing while nothing
/// plays, a closed game's process sends nothing), so its track gets silence up to the clock.
const IDLE: std::time::Duration = std::time::Duration::from_millis(200);
/// Event wait per turn: short, so a quiet source is filled promptly.
const WAIT_MS: u32 = 100;

/// One open capture stream.
struct Stream {
    client: wasapi::AudioClient,
    event: wasapi::Handle,
    capture: wasapi::AudioCaptureClient,
    endpoint: String,
}

/// Open `target`'s process loopback, else the source's endpoint, and start it. All COM
/// objects live on the calling (capture) thread.
fn open_stream(direction: &Direction, device: &str, ring: &TrackRing, target: &Option<LoopbackTarget>) -> Result<Stream, String> {
    let format = WaveFormat::new(32, 32, &SampleType::Float, ring.rate as usize, ring.channels as usize, None);
    let (mut client, endpoint, buffer_duration_hns) = match target {
        Some(LoopbackTarget { pid, label }) => (
            wasapi::AudioClient::new_application_loopback_client(*pid, true).map_err(|e| format!("process loopback for PID {pid}: {e}"))?,
            label.clone(),
            // Process-loopback clients do not support get_device_period.
            // 20 ms is the wasapi example's documented safe event period.
            200_000,
        ),
        None => {
            let (device, endpoint) = pick_device(direction, device, ring.input)?;
            let client = device.get_iaudioclient().map_err(|e| e.to_string())?;
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
    Ok(Stream { client, event, capture, endpoint })
}

/// Silence up to the wall clock (the source sends nothing right now).
fn fill_silence(ring: &TrackRing, level: &mut apps::Leveller, started: std::time::Instant) {
    let frames = level.idle(started.elapsed());
    if frames > 0 {
        ring.push_capture(&vec![0.0; frames as usize * ring.channels as usize], false);
    }
}

/// Capture until the stream fails. Every packet goes through the track's `Leveller`, so
/// the ring stays level with the wall clock: a save cuts every track on the same clock
/// (`encoder::handoff`), quiet stretches included. Returns why the stream ended.
fn run_stream(ring: &TrackRing, stream: &Stream, level: &mut apps::Leveller, started: std::time::Instant, device: bool) -> String {
    let block = ring.channels as usize * 4; // f32 frame size in bytes
    let channels = ring.channels as usize;
    let mut raw: VecDeque<u8> = VecDeque::new();
    let mut last_sound = std::time::Instant::now();
    loop {
        if stream.event.wait_for_event(WAIT_MS).is_ok() {
            if let Err(e) = stream.capture.read_from_device_to_deque(&mut raw) {
                return format!("read failed: {e}");
            }
            if raw.len() >= block {
                let full = raw.len() - raw.len() % block;
                let samples: Vec<f32> = raw.drain(..full).collect::<Vec<u8>>().chunks_exact(4).map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]])).collect();
                ring.push_capture(&level.live(started.elapsed(), &samples, channels), mic_muted());
                last_sound = std::time::Instant::now();
                continue;
            }
        } else if device {
            // A device that was unplugged or invalidated answers this with an error.
            if let Err(e) = stream.client.get_current_padding() {
                return format!("device lost: {e}");
            }
        }
        if last_sound.elapsed() >= IDLE {
            fill_silence(ring, level, started);
        }
    }
}

fn set_endpoint(ring: &TrackRing, text: String) {
    if let Ok(mut g) = ring.endpoint.lock() {
        *g = text;
    }
}

/// The capture thread of a device or game track: open, capture, and when the device or
/// stream is lost, keep the track level with silence and reopen every `REOPEN_EVERY`.
/// Only the first open is reported to `start_track` (a source that never opens has no track).
fn capture_thread(
    direction: Direction,
    device: String,
    ring: Arc<TrackRing>,
    init_tx: std::sync::mpsc::Sender<Result<(), String>>,
    target: Option<LoopbackTarget>,
) {
    // Per-thread COM init; already-initialized is fine.
    let _ = wasapi::initialize_mta();
    let started = std::time::Instant::now();
    let mut level = apps::Leveller::new(ring.rate);
    let mut init_tx = Some(init_tx);
    loop {
        match open_stream(&direction, &device, &ring, &target) {
            Ok(stream) => {
                set_endpoint(&ring, stream.endpoint.clone());
                if let Some(tx) = init_tx.take() {
                    let _ = tx.send(Ok(()));
                }
                let why = run_stream(&ring, &stream, &mut level, started, target.is_none());
                let _ = stream.client.stop_stream();
                eprintln!("[recall] audio track '{}': {why}; reopening", ring.name);
                set_endpoint(&ring, format!("{} (lost: {why}; reopening)", stream.endpoint));
            }
            Err(e) => {
                if let Some(tx) = init_tx.take() {
                    let _ = tx.send(Err(e));
                    return;
                }
            }
        }
        let until = std::time::Instant::now() + REOPEN_EVERY;
        while std::time::Instant::now() < until {
            std::thread::sleep(std::time::Duration::from_millis(100));
            fill_silence(&ring, &mut level, started);
        }
    }
}

#[cfg(test)]
#[path = "audio_tests.rs"]
mod tests;
