//! WASAPI endpoint capture for the replay buffer (feature `encode`).
//!
//! Endpoint/virtual-cable model (PRD §9): a track = one audio ENDPOINT.
//! - "Game": LOOPBACK capture of a render endpoint (Sonar Gaming preferred).
//! - "Mic": normal capture of a capture endpoint (Sonar Microphone preferred).
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

/// Selection preference for one track: substring matched against endpoint
/// friendly names (case-insensitive); falls back to the default endpoint.
pub struct TrackSpec {
    pub name: &'static str,
    pub direction: Direction,
    /// Env var holding a name-substring override.
    pub env: &'static str,
    /// Default name-substring preferences, tried in order.
    pub prefer: &'static [&'static str],
}

pub static TRACKS: [TrackSpec; 2] = [
    TrackSpec {
        name: "Game",
        direction: Direction::Render, // loopback
        env: "RECALL_AUDIO_GAME",
        // Token-AND matching: every whitespace-separated token must appear in
        // the friendly name. This machine's cable set is the RØDECaster
        // virtual suite (probed 2026-07-08): games route to "Game (RØDECaster)".
        prefer: &["game rødecaster", "game rodecaster", "sonar gaming"],
    },
    TrackSpec {
        name: "Mic",
        direction: Direction::Capture,
        env: "RECALL_AUDIO_MIC",
        prefer: &[
            "microphone rodecaster chat",
            "microphone rødecaster chat",
            "sonar microphone",
        ],
    },
];

/// One track's PCM captured for a save: interleaved f32, front-padded to the
/// clip length. Owned and `Send`, so the saver thread encodes it off-ring.
pub struct AudioPcm {
    pub name: &'static str,
    pub rate: u32,
    pub channels: u16,
    pub samples: Vec<f32>,
}

/// Interleaved f32 ring + the format it was captured in.
pub struct TrackRing {
    pub name: &'static str,
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
    let mut out = Vec::new();
    for spec in &TRACKS {
        match start_track(spec, buffer_secs) {
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

/// True when every whitespace-separated token of `want` appears in `name`
/// (both lowercased) — survives vendor naming like
/// "Gaming (SteelSeries Sonar Virtual Audio Device)".
fn name_matches(name: &str, want: &str) -> bool {
    let name = name.to_lowercase();
    want.split_whitespace()
        .all(|tok| name.contains(&tok.to_lowercase()))
}

/// Pick the endpoint for a spec: env override tokens, then preference list,
/// then the OS default for that direction.
fn pick_device(spec: &TrackSpec) -> Result<(wasapi::Device, String), String> {
    let collection =
        DeviceCollection::new(&spec.direction).map_err(|e| format!("enumerate: {e}"))?;
    let n = collection.get_nbr_devices().map_err(|e| e.to_string())?;
    let mut wanted: Vec<String> = Vec::new();
    if let Ok(over) = std::env::var(spec.env) {
        wanted.push(over);
    }
    wanted.extend(spec.prefer.iter().map(|s| s.to_string()));

    for want in &wanted {
        for i in 0..n {
            let Ok(dev) = collection.get_device_at_index(i) else { continue };
            let name = dev.get_friendlyname().unwrap_or_default();
            if name_matches(&name, want) {
                return Ok((dev, name));
            }
        }
    }
    // Fall back to the default endpoint for the direction.
    let dev = wasapi::get_default_device(&spec.direction)
        .map_err(|e| format!("no preference matched; default endpoint failed: {e}"))?;
    let name = dev.get_friendlyname().unwrap_or_else(|_| "(default)".into());
    Ok((dev, format!("{name} (default fallback)")))
}

/// Resolve the opt-in process-loopback target for Game capture. A supplied
/// value deliberately never falls back to endpoint capture: that could record
/// the wrong audio source while appearing to work.
fn game_process_loopback_pid(spec: &TrackSpec) -> Result<Option<u32>, String> {
    if spec.name != "Game" {
        return Ok(None);
    }
    let value = match std::env::var("RECALL_AUDIO_GAME_PID") {
        Ok(value) => value,
        Err(std::env::VarError::NotPresent) => return Ok(None),
        Err(std::env::VarError::NotUnicode(_)) => {
            return Err("RECALL_AUDIO_GAME_PID is not valid Unicode".into());
        }
    };
    let pid = value
        .trim()
        .parse::<u32>()
        .map_err(|_| format!("RECALL_AUDIO_GAME_PID must be a positive process ID, got {value:?}"))?;
    if pid == 0 {
        return Err("RECALL_AUDIO_GAME_PID must be a positive process ID, got 0".into());
    }
    Ok(Some(pid))
}

/// Spawn the capture thread for one track. ALL WASAPI/COM objects are created
/// on the capture thread (they are !Send); init success/failure is reported
/// back over a channel before this returns.
fn start_track(spec: &'static TrackSpec, buffer_secs: u32) -> Result<Arc<TrackRing>, String> {
    // Fixed 48k stereo f32 for every track; WASAPI autoconverts from the
    // endpoint's native mix format.
    let (rate, channels) = (48_000u32, 2u16);
    let ring = Arc::new(TrackRing {
        name: spec.name,
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
        .spawn(move || capture_thread(spec, tring, init_tx))
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
        let (mut client, endpoint, buffer_duration_hns) = match game_process_loopback_pid(spec)? {
            Some(pid) => (
                wasapi::AudioClient::new_application_loopback_client(pid, true)
                    .map_err(|e| format!("process loopback for PID {pid}: {e}"))?,
                format!("process {pid} (include tree)"),
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
