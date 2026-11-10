//! WASAPI capture for the replay buffer (feature `encode`).
//!
//! A track = one recorded source (`sources.rs`, the Audio page's strips): the game's process
//! tree, an output device by loopback, a microphone or line-in, or an app's process tree
//! (`apps.rs`). Each is titled with the source's name, scaled by its gain before anything is
//! stored, and the mic-mute hotkey silences every input unless its source says it is a line-in
//! or capture card (`SourceSpec::mic_mute = false`) on a named device. A named device only ever records itself: while it is not connected its track is silent and the status says
//! "<name> (not connected; waiting)" (`sources::opened`); a source with no device follows the
//! Windows default as it moves (`notify.rs`).
//!
//! Each track runs its own capture thread pushing interleaved f32 samples into a ring
//! holding the last `buffer_secs`. Every track is kept level with the wall clock
//! (`apps::Leveller`): silence while its source sends nothing (loopback of a quiet output,
//! a closed game), and a lost device is reopened every `REOPEN_EVERY` meanwhile; a device
//! not there at start (named, or no Windows default yet) waits for it the same way (`capture_loop`). Gain and
//! name change live (`sources::set_live`), so the Audio page never restarts the engine. The encoder snapshots rings at save time and encodes AAC
//! per track — raw PCM in RAM keeps encoder state out of the ring (~23MB / 60s / stereo).

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

use wasapi::{DeviceCollection, Direction};

mod apps;
mod capture_loop;
mod endpoints;
mod hold;
mod mute;
mod notify;
mod packet;
mod rebind;
pub mod sources;
mod stream;
mod voice;

use sources::{Kind, SourceSpec};

#[cfg(test)]
pub(crate) use endpoints::{choose_endpoint, Pick};
#[cfg(test)]
use endpoints::match_endpoint;

pub(crate) use mute::{initial_mute, MuteNow};
#[cfg(test)]
pub(crate) use mute::MIC_MUTED_ENV;
pub use mute::{mic_muted, set_mic_muted, set_mic_muted_by, toggle_mic_mute};
pub(crate) use hold::{is_device_source, starts_held};

/// The spawn-time mute (`RECALL_MIC_MUTED`), applied before stdin is read and before any track
/// opens: a muted mic records nothing of this run, and an Alt+M that comes early is kept.
pub fn apply_spawn_mute() {
    set_mic_muted(initial_mute(|key| std::env::var(key).ok()));
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
    /// An input device (opens a capture endpoint).
    input: bool,
    /// The mic-mute hotkey silences this input: `SourceSpec::mic_mute`, absent = true (every
    /// config written before the flag, the default mic), whatever Windows calls the endpoint.
    mic_mute: AtomicBool,
    /// The source names no device: it records the Windows default input, normally the user's
    /// microphone, so Alt+M mutes it whatever `mic_mute` says (R4-deep-audio-1). A device
    /// change restarts the engine, so this is fixed for the ring's life.
    on_default: bool,
    /// The open stream is the source's own saved device (`endpoints::opened_own`); false
    /// before the first open and between streams. A line-in on any other endpoint follows Alt+M.
    opened_own: AtomicBool,
    /// Resolved endpoint friendly name (set by the capture thread at init).
    endpoint: Mutex<String>,
    pub rate: u32,
    pub channels: u16,
    samples: Mutex<VecDeque<f32>>,
    capacity: usize,
    /// The saved id, an unsure sibling's hold and the re-pick slot (`hold.rs`).
    live: hold::Live,
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
            mic_mute: AtomicBool::new(spec.mic_mute.unwrap_or(true)),
            on_default: spec.device.trim().is_empty(),
            opened_own: AtomicBool::new(false),
            endpoint: Mutex::new(endpoint),
            rate: RATE,
            channels: CHANNELS,
            // The whole buffer up front: growing by doubling wasted up to ~45% and copied the
            // ring (up to ~134 MB) under the lock a save and the capture thread share.
            samples: Mutex::new(VecDeque::with_capacity(buffer_secs as usize * RATE as usize * CHANNELS as usize)),
            capacity: buffer_secs as usize * RATE as usize * CHANNELS as usize,
            live: hold::Live::with_saved_id(&spec.device_id),
        }
    }

    /// A 48 kHz stereo game-audio ring of `buffer_secs` fed by the test (no WASAPI).
    #[cfg(test)]
    pub(crate) fn for_test(name: &'static str, buffer_secs: u32) -> Self {
        let spec = SourceSpec { communications: false, device_id: String::new(), id: name.into(), kind: "game".into(), device: String::new(), exe: String::new(), name: name.into(), gain_db: 0.0, mic_mute: None };
        Self::for_source(&spec, "game", buffer_secs, String::new())
    }

    /// Append samples as the capture thread does.
    #[cfg(test)]
    pub(crate) fn push_for_test(&self, data: &[f32]) {
        self.push_capture(data, false);
    }

    /// Whether the mic-mute hotkey silences this track from now on (an input only): the
    /// per-strip toggle.
    pub fn set_mic_mute(&self, on: bool) {
        self.mic_mute.store(on, Ordering::Relaxed);
    }

    /// The mic-mute hotkey silences this track: an input, unless its source says it is a line-in
    /// or capture card on a named device and the stream is that device itself (SPEC A12,
    /// R3/R4/R5-deep-audio-1). The Windows default and any other endpoint are always muted.
    fn mutes(&self) -> bool {
        self.input && (self.mic_mute.load(Ordering::Relaxed) || self.on_default || !self.opened_own.load(Ordering::Relaxed))
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
    /// only microphone inputs, and only samples entering the ring: replay history never changes.
    ///
    /// A block bigger than the ring keeps only its newest `capacity` samples, and the ring
    /// makes room before it takes them, so it never grows past its capacity.
    fn push_capture(&self, data: &[f32], mute_inputs: bool) {
        self.push_samples(data, mute_inputs, false);
    }

    /// [`Self::push_capture`]; `held` stores silence whatever Alt+M says (`hold.rs`).
    fn push_samples(&self, data: &[f32], mute_inputs: bool, held: bool) {
        if crate::stop_capture::stopped() {
            return; // the privacy stop: no sample enters the ring after it
        }
        // The gained copy reuses one buffer per capture thread (gate review P2-c).
        thread_local!(static GAINED: std::cell::RefCell<Vec<f32>> = const { std::cell::RefCell::new(Vec::new()) });
        let data = &data[data.len().saturating_sub(self.capacity)..];
        let silence = held || (mute_inputs && self.mutes());
        let gain = self.gain();
        if gain != 1.0 && !silence {
            return GAINED.with(|g| {
                let mut gained = g.borrow_mut();
                gained.clear();
                gained.extend(data.iter().map(|s| s * gain));
                self.store(&gained, false);
            });
        }
        self.store(data, silence);
    }

    /// `data` (gain applied) into the ring and the long-recording tap; `silence` stores zeros.
    fn store(&self, data: &[f32], silence: bool) {
        crate::longrec::tap::tap_audio(self.name, data, silence); // long-recording hook
        let mut buf = self.samples.lock().expect("ring lock");
        let over = (buf.len() + data.len()).saturating_sub(self.capacity);
        if over > 0 {
            buf.drain(..over);
        }
        for &s in data {
            buf.push_back(if silence { 0.0 } else { s });
        }
    }
}

/// Resolve + start every source in list order. Failures are per-source (a broken input
/// must not kill game audio); returns whatever started, with a report line each.
/// `hold`: every device track starts held (`TrackRing::hold_startup`) until the controller's
/// `sync` (`sources::sync_read`); a process-loopback or app track records at once.
pub fn start(buffer_secs: u32, hold: bool, mut report: impl FnMut(String)) -> Vec<Arc<TrackRing>> {
    // The spawn mute was applied before stdin is read (`apply_spawn_mute`, main.rs).
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
        match start_track(spec, buffer_secs, target, hold) {
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
    sources::close_staging(); // every track is listed: later set_source lines apply at once
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
fn start_track(spec: &SourceSpec, buffer_secs: u32, target: Option<LoopbackTarget>, hold: bool) -> Result<Arc<TrackRing>, String> {
    let ring = Arc::new(TrackRing::for_source(spec, source_of(spec.kind(), target.is_some()), buffer_secs, String::new()));
    if starts_held(hold, target.is_some()) {
        ring.hold_startup(); // before its capture thread starts: no stream opens while held (F4)
    }
    let direction = if spec.kind() == Some(Kind::Input) { Direction::Capture } else { Direction::Render };
    let device = (spec.device.clone(), spec.device_id.clone());
    let communications = spec.uses_communications();
    let tring = ring.clone();
    let (init_tx, init_rx) = std::sync::mpsc::channel::<Result<(), String>>();
    std::thread::Builder::new()
        .name(format!("recall-audio-{}", ring.name))
        .spawn(move || capture_thread(direction, device, tring, init_tx, target, communications))
        .map_err(|e| e.to_string())?;

    hold::await_init(&ring, &init_rx, std::time::Duration::from_secs(5)).map(|()| ring)
}

/// The capture thread of a device or game track (`capture_loop`): open, capture, and when
/// the device or stream is lost, keep the track level with silence and reopen. A device that is
/// not there at start still gets its track and waits for it (by name, or for a Windows default).
fn capture_thread(
    direction: Direction,
    (device, _): (String, String),
    ring: Arc<TrackRing>,
    init_tx: std::sync::mpsc::Sender<Result<(), String>>,
    target: Option<LoopbackTarget>,
    communications: bool,
) {
    // Per-thread COM init; already-initialized is fine.
    let _ = wasapi::initialize_mta();
    stream::join_mmcss_audio(ring.name);
    let exact = std::env::var_os("RECALL_AUDIO_EXACT").is_some();
    let wait_absent = capture_loop::waits_when_absent(target.is_some(), exact);
    let mut clock = capture_loop::WallClock::start();
    // Device and default changes arrive as notices (no polling on this thread); without them
    // the track still reopens on its backoff.
    // A process-loopback track follows its process, not an endpoint: no notices.
    let notices = capture_loop::notices_for(target.is_some(), || notify::EndpointNotices::register(direction, communications));
    if let Some(Err(e)) = &notices {
        eprintln!("[recall] audio track '{}': no device notices ({e}); reopening on the backoff only", ring.name);
    }
    let notices: &dyn capture_loop::Notices = match &notices {
        Some(Ok(n)) => n,
        _ => &capture_loop::NoNotices,
    };
    // The saved id as the controller sends it now (`set_source device_id`, P1-B).
    let open = |_reopen| stream::open_stream(&direction, (&device, &ring.live.saved_id()), &ring, &target, communications);
    capture_loop::capture_loop(&ring, &device, wait_absent, open, init_tx, &mut clock, notices);
}

#[cfg(test)]
#[path = "audio_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "audio_sleep_tests.rs"]
mod sleep_tests;
