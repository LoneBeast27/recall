//! WASAPI endpoint capture for the replay buffer (feature `encode`).
//!
//! Endpoint/virtual-cable model (PRD §9): a track = one audio ENDPOINT or one
//! process tree.
//! - "Game": process loopback of `RECALL_AUDIO_GAME_PID` (incl. children), else
//!   LOOPBACK capture of the render endpoint named by `RECALL_AUDIO_GAME`
//!   (Settings → Desktop audio), else the Windows default output.
//! - "Mic": normal capture of the endpoint named by `RECALL_AUDIO_MIC`
//!   (Settings → Microphone), else the Windows default mic. No Mic track at
//!   all when `RECALL_AUDIO_MIC_OFF=1`.
//! - "Voice": opt-in process loopback of a voice-chat app's process tree
//!   (`RECALL_AUDIO_VOICE_PID`, or `RECALL_AUDIO_VOICE_EXE` e.g. `Discord.exe`);
//!   no track when neither is set or the app is not running. Mic mute never
//!   touches it.
//! - App tracks: one per app chosen in Settings (`RECALL_AUDIO_APPS`), after
//!   the tracks above, named by the person's label; see `audio/apps.rs`.
//! App→cable routing stays the user's/Windows' job; we only read endpoints.
//!
//! Each track runs its own capture thread pushing interleaved f32 samples into
//! a ring holding the last `buffer_secs`. The encoder snapshots rings at save
//! time and encodes AAC per track — raw PCM in RAM keeps encoder state out of
//! the ring (~23MB / 60s / stereo track).

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use wasapi::{DeviceCollection, Direction, SampleType, StreamMode, WaveFormat};

mod apps;
mod voice;

static MIC_MUTED: AtomicBool = AtomicBool::new(false);

/// Set whether newly captured microphone samples enter the replay buffer as silence.
pub fn set_mic_muted(muted: bool) {
    MIC_MUTED.store(muted, Ordering::Relaxed);
}

/// Toggle microphone capture muting and return the new state.
pub fn toggle_mic_mute() -> bool {
    MIC_MUTED.fetch_xor(true, Ordering::Relaxed) ^ true
}

/// Return whether newly captured microphone samples are muted.
pub fn mic_muted() -> bool {
    MIC_MUTED.load(Ordering::Relaxed)
}

/// Where a track's process-loopback target comes from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProcessSource {
    /// Endpoint capture only.
    None,
    /// `RECALL_AUDIO_GAME_PID`'s process tree when set, else the endpoint.
    GamePid,
    /// The voice-chat app's process tree only; no track when unresolved.
    VoiceApp,
}

/// One track: the configured endpoint name (exact, then token-AND match),
/// else the Windows default endpoint for the direction.
pub struct TrackSpec {
    /// Internal name (Moments `audio_tracks`, mute rule, thread name).
    pub name: &'static str,
    /// Saved as the stream's `title` and `handler_name`.
    pub title: &'static str,
    pub direction: Direction,
    /// Env var holding the configured endpoint name (endpoint tracks only).
    pub env: &'static str,
    pub process: ProcessSource,
}

pub static TRACKS: [TrackSpec; 3] = [
    TrackSpec {
        name: "Game",
        title: "Game",
        direction: Direction::Render, // loopback
        env: "RECALL_AUDIO_GAME",
        process: ProcessSource::GamePid,
    },
    TrackSpec {
        name: "Mic",
        title: "Microphone",
        direction: Direction::Capture,
        env: "RECALL_AUDIO_MIC",
        process: ProcessSource::None,
    },
    TrackSpec {
        name: "Voice",
        title: "Voice chat",
        direction: Direction::Render,
        env: "",
        process: ProcessSource::VoiceApp,
    },
];

/// Stream title for a track name ("Mic" -> "Microphone"); unknown names pass
/// through unchanged.
pub fn track_title(name: &str) -> &str {
    TRACKS
        .iter()
        .find(|t| t.name == name)
        .map(|t| t.title)
        .unwrap_or(name)
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
    /// Where the track came from (`edit_model::SOURCE_*` / `app:<exe>`), saved in the clip.
    pub source: &'static str,
    pub rate: u32,
    pub channels: u16,
    pub samples: Vec<f32>,
}

/// Interleaved f32 ring + the format it was captured in.
pub struct TrackRing {
    pub name: &'static str,
    /// Where the track comes from (`edit_model::SOURCE_*` / `app:<exe>`).
    pub source: &'static str,
    /// Resolved endpoint friendly name (set by the capture thread at init).
    endpoint: Mutex<String>,
    pub rate: u32,
    pub channels: u16,
    samples: Mutex<VecDeque<f32>>,
    capacity: usize,
}

impl TrackRing {
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
            source: self.source,
            rate: self.rate,
            channels: self.channels,
            samples: self.snapshot(secs),
        }
    }

    /// Append captured samples. Muting is applied only to microphone samples
    /// entering the ring, so existing replay history is never changed.
    fn push_capture(&self, data: &[f32], mute_mic: bool) {
        crate::longrec::tap::tap_audio(self.name, data, mute_mic && self.name == "Mic"); // long-recording hook
        let mut buf = self.samples.lock().expect("ring lock");
        for &s in data {
            let s = if mute_mic && self.name == "Mic" { 0.0 } else { s };
            buf.push_back(s);
        }
        let over = buf.len().saturating_sub(self.capacity);
        if over > 0 {
            buf.drain(..over);
        }
    }
}

/// Resolve + start all tracks. Failures are per-track (a broken Mic endpoint
/// must not kill Game audio); returns whatever started with a report line each.
pub fn start(buffer_secs: u32, mut report: impl FnMut(String)) -> Vec<Arc<TrackRing>> {
    report(format!(
        "audio endpoints -- render: {:?} | capture: {:?}",
        list_endpoints(&Direction::Render),
        list_endpoints(&Direction::Capture)
    ));
    let game_pid = game_process_loopback_pid();
    let mut out = Vec::new();
    for spec in &TRACKS {
        if spec.name == "Mic" && mic_off() {
            report(format!("audio track '{}' off: microphone turned off in Settings", spec.name));
            continue;
        }
        let target = match spec.process {
            ProcessSource::None => None,
            ProcessSource::GamePid => match &game_pid {
                Ok(pid) => pid.map(|pid| LoopbackTarget {
                    pid,
                    label: format!("process {pid} (include tree)"),
                }),
                Err(e) => {
                    report(format!("audio track '{}' UNAVAILABLE: {e}", spec.name));
                    continue;
                }
            },
            ProcessSource::VoiceApp => {
                match voice::resolve_from_env(game_pid.clone().ok().flatten()) {
                    Ok(voice::Resolved::Target(target)) => Some(target),
                    // Not an error: the voice track is opt-in.
                    Ok(voice::Resolved::Off(reason)) => {
                        report(format!("audio track '{}' off: {reason}", spec.name));
                        continue;
                    }
                    Err(e) => {
                        report(format!("audio track '{}' UNAVAILABLE: {e}", spec.name));
                        continue;
                    }
                }
            }
        };
        match start_track(spec, buffer_secs, target) {
            Ok(ring) => {
                report(format!(
                    "audio track '{}' up: {} ({} Hz, {} ch)",
                    ring.name,
                    ring.endpoint(),
                    ring.rate,
                    ring.channels
                ));
                out.push(ring);
            }
            Err(e) => report(format!("audio track '{}' UNAVAILABLE: {e}", spec.name)),
        }
    }
    match apps::from_env() {
        Ok(list) => out.extend(apps::start(&list, buffer_secs, game_pid.clone().ok().flatten(), &mut report)),
        Err(e) => report(format!("app audio tracks UNAVAILABLE: {e}")),
    }
    out
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

/// `RECALL_AUDIO_MIC_OFF=1`: the person turned the microphone off in
/// Settings, so no Mic track is captured at all.
fn mic_off() -> bool {
    std::env::var("RECALL_AUDIO_MIC_OFF").is_ok_and(|v| v.trim() == "1")
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

/// Pick the endpoint for a spec: the configured name (env), else the Windows
/// default for that direction, with the reason in the reported name.
/// `RECALL_AUDIO_EXACT` allows the configured name only: a test run gets its
/// virtual endpoint or no track, never the person's real mic.
fn pick_device(spec: &TrackSpec) -> Result<(wasapi::Device, String), String> {
    let collection =
        DeviceCollection::new(&spec.direction).map_err(|e| format!("enumerate: {e}"))?;
    let n = collection.get_nbr_devices().map_err(|e| e.to_string())?;
    let exact = std::env::var_os("RECALL_AUDIO_EXACT").is_some();
    let want = std::env::var(spec.env).ok().filter(|w| !w.trim().is_empty());

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
    let dev = wasapi::get_default_device(&spec.direction)
        .map_err(|e| format!("default endpoint failed: {e}"))?;
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

/// The saved source of a built-in track: the game's process tree, or an output it listens to.
fn source_of(spec: &TrackSpec, process: bool) -> &'static str {
    use crate::edit_model::{SOURCE_DESKTOP, SOURCE_GAME_PROCESS, SOURCE_MIC, SOURCE_VOICE};
    match spec.process {
        ProcessSource::GamePid if process => SOURCE_GAME_PROCESS,
        ProcessSource::GamePid => SOURCE_DESKTOP,
        ProcessSource::VoiceApp => SOURCE_VOICE,
        ProcessSource::None if spec.direction == Direction::Capture => SOURCE_MIC,
        ProcessSource::None => SOURCE_DESKTOP,
    }
}

/// Spawn the capture thread for one track. ALL WASAPI/COM objects are created
/// on the capture thread (they are !Send); init success/failure is reported
/// back over a channel before this returns. `target` selects process loopback
/// instead of the spec's endpoint.
fn start_track(
    spec: &'static TrackSpec,
    buffer_secs: u32,
    target: Option<LoopbackTarget>,
) -> Result<Arc<TrackRing>, String> {
    // Fixed 48k stereo f32 for every track; WASAPI autoconverts from the
    // endpoint's native mix format.
    let (rate, channels) = (48_000u32, 2u16);
    let ring = Arc::new(TrackRing {
        name: spec.name,
        source: source_of(spec, target.is_some()),
        endpoint: Mutex::new(String::new()),
        rate,
        channels,
        samples: Mutex::new(VecDeque::new()),
        capacity: buffer_secs as usize * rate as usize * channels as usize,
    });

    let tring = ring.clone();
    let (init_tx, init_rx) = std::sync::mpsc::channel::<Result<(), String>>();
    std::thread::Builder::new()
        .name(format!("recall-audio-{}", spec.name))
        .spawn(move || capture_thread(spec, tring, init_tx, target))
        .map_err(|e| e.to_string())?;

    match init_rx.recv_timeout(std::time::Duration::from_secs(5)) {
        Ok(Ok(())) => Ok(ring),
        Ok(Err(e)) => Err(e),
        Err(_) => Err("audio init timed out".into()),
    }
}

fn capture_thread(
    spec: &'static TrackSpec,
    ring: Arc<TrackRing>,
    init_tx: std::sync::mpsc::Sender<Result<(), String>>,
    target: Option<LoopbackTarget>,
) {
    let init = || -> Result<_, String> {
        // Per-thread COM init; already-initialized is fine.
        let _ = wasapi::initialize_mta();
        let format = WaveFormat::new(
            32,
            32,
            &SampleType::Float,
            ring.rate as usize,
            ring.channels as usize,
            None,
        );
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
                let (device, endpoint) = pick_device(spec)?;
                let mut client = device.get_iaudioclient().map_err(|e| e.to_string())?;
                let (_def_period, min_period) =
                    client.get_device_period().map_err(|e| e.to_string())?;
                (client, endpoint, min_period)
            }
        };
        let mode = StreamMode::EventsShared {
            autoconvert: true,
            buffer_duration_hns,
        };
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
