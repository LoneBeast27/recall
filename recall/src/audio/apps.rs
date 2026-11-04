//! App tracks: each app the person chose in Settings (`RECALL_AUDIO_APPS`, a JSON
//! array of `{exe, label}`) is recorded as a track of its own, captured by process
//! loopback of the app's process tree (include-tree mode, like the voice track).
//!
//! Every chosen app gets its track for the whole recording, running or not, so a
//! clip's track count and order never depend on which apps were open: a watcher
//! thread per app finds the app's process (rescanning every 2 s while it is closed),
//! captures it while it runs, and writes silence by the wall clock whenever the app
//! is closed or quiet, so the track stays aligned with the others.
//!
//! Not done: keeping a captured app out of the desktop track. Process loopback can
//! exclude only one process tree per stream and ignores the chosen output device,
//! so an app on its own track is also heard in Desktop audio.

use std::collections::VecDeque;
use std::ffi::c_void;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde::Deserialize;
use wasapi::{Direction, SampleType, StreamMode, WaveFormat};

use super::voice::{snapshot_processes, tree_root};
use super::TrackRing;

pub const APPS_ENV: &str = "RECALL_AUDIO_APPS";
/// Same cap as the Settings list.
const MAX_APPS: usize = 6;
/// A closed app is looked for this often.
const RESCAN: Duration = Duration::from_secs(2);
/// Silence is written when the track falls this far behind the wall clock.
const SLACK: Duration = Duration::from_millis(100);

#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
pub struct AppSource {
    pub exe: String,
    #[serde(default)]
    pub label: String,
}

impl AppSource {
    /// The track's name in clips: the label, else the exe name without `.exe`.
    pub fn track_name(&self) -> String {
        let label = self.label.trim();
        if !label.is_empty() {
            return label.to_owned();
        }
        let exe = self.exe.trim();
        let stem = exe.len().checked_sub(4).filter(|&n| exe[n..].eq_ignore_ascii_case(".exe")).map_or(exe, |n| &exe[..n]);
        stem.to_owned()
    }
}

/// The chosen apps; none when the variable is unset or empty.
pub fn from_env() -> Result<Vec<AppSource>, String> {
    match std::env::var(APPS_ENV) {
        Ok(value) if !value.trim().is_empty() => parse(&value),
        Ok(_) | Err(std::env::VarError::NotPresent) => Ok(Vec::new()),
        Err(std::env::VarError::NotUnicode(_)) => Err(format!("{APPS_ENV} is not valid Unicode")),
    }
}

pub fn parse(json: &str) -> Result<Vec<AppSource>, String> {
    let apps: Vec<AppSource> = serde_json::from_str(json).map_err(|e| format!("{APPS_ENV}: {e}"))?;
    if apps.len() > MAX_APPS {
        return Err(format!("{APPS_ENV} lists {} apps; at most {MAX_APPS}", apps.len()));
    }
    if let Some(bad) = apps.iter().find(|a| a.exe.trim().is_empty() || a.exe.contains(['\\', '/', ':'])) {
        return Err(format!("{APPS_ENV}: '{}' is not an exe file name", bad.exe));
    }
    Ok(apps)
}

/// One ring and one watcher thread per app, in list order.
pub fn start(apps: &[AppSource], buffer_secs: u32, report: &mut impl FnMut(String)) -> Vec<Arc<TrackRing>> {
    let mut out = Vec::new();
    for (n, app) in apps.iter().enumerate() {
        // Track names are `&'static str` across the engine; an engine run holds at most
        // MAX_APPS of these for its whole life, so leaking them is bounded.
        let name: &'static str = Box::leak(app.track_name().into_boxed_str());
        let (rate, channels) = (48_000u32, 2u16);
        let ring = Arc::new(TrackRing {
            name,
            endpoint: Mutex::new(format!("{} (not running)", app.exe.trim())),
            rate,
            channels,
            samples: Mutex::new(VecDeque::new()),
            capacity: buffer_secs as usize * rate as usize * channels as usize,
        });
        let (thread_ring, exe) = (Arc::clone(&ring), app.exe.trim().to_owned());
        match std::thread::Builder::new().name(format!("recall-app-audio-{n}")).spawn(move || watch(thread_ring, exe)) {
            Ok(_) => {
                report(format!("audio track '{name}' up: app {} (silent while it is closed)", app.exe.trim()));
                out.push(ring);
            }
            Err(e) => report(format!("audio track '{name}' UNAVAILABLE: {e}")),
        }
    }
    out
}

/// Keeps a track level with the wall clock: counts the frames written and says how many
/// frames of silence are owed once the track falls more than `slack` behind.
pub struct SilenceClock {
    rate: u32,
    written: u64,
}

impl SilenceClock {
    pub fn new(rate: u32) -> Self {
        Self { rate, written: 0 }
    }

    pub fn wrote(&mut self, frames: u64) {
        self.written += frames;
    }

    /// Frames of silence to write now, `elapsed` after the track started.
    pub fn owed(&mut self, elapsed: Duration, slack: Duration) -> u64 {
        let due = (elapsed.as_secs_f64() * self.rate as f64) as u64;
        let slack = (slack.as_secs_f64() * self.rate as f64) as u64;
        if self.written + slack >= due {
            return 0;
        }
        let owed = due - self.written;
        self.written = due;
        owed
    }
}

fn pad(ring: &TrackRing, clock: &mut SilenceClock, started: Instant, slack: Duration) {
    let owed = clock.owed(started.elapsed(), slack);
    if owed > 0 {
        ring.push_capture(&vec![0.0; owed as usize * ring.channels as usize], false);
    }
}

fn set_endpoint(ring: &TrackRing, text: String) {
    if let Ok(mut endpoint) = ring.endpoint.lock() {
        *endpoint = text;
    }
}

/// The app's watcher thread: capture while it runs, silence while it does not.
fn watch(ring: Arc<TrackRing>, exe: String) {
    let _ = wasapi::initialize_mta();
    let started = Instant::now();
    let mut clock = SilenceClock::new(ring.rate);
    loop {
        if let Some(pid) = snapshot_processes().ok().and_then(|procs| tree_root(&procs, &exe)) {
            set_endpoint(&ring, format!("{exe} pid {pid} (include tree)"));
            if let Err(e) = capture_while_running(&ring, pid, &mut clock, started) {
                set_endpoint(&ring, format!("{exe} pid {pid}: {e}"));
            } else {
                set_endpoint(&ring, format!("{exe} (closed)"));
            }
        }
        let until = Instant::now() + RESCAN;
        while Instant::now() < until {
            std::thread::sleep(Duration::from_millis(100));
            pad(&ring, &mut clock, started, Duration::ZERO);
        }
    }
}

/// Captures `pid`'s process tree until the process exits. Errors end the capture;
/// the watcher writes silence and looks for the app again.
fn capture_while_running(ring: &TrackRing, pid: u32, clock: &mut SilenceClock, started: Instant) -> Result<(), String> {
    let process = Process::open(pid).ok_or_else(|| format!("open process: {}", std::io::Error::last_os_error()))?;
    let mut client = wasapi::AudioClient::new_application_loopback_client(pid, true).map_err(|e| format!("process loopback: {e}"))?;
    let format = WaveFormat::new(32, 32, &SampleType::Float, ring.rate as usize, ring.channels as usize, None);
    // Process-loopback clients have no device period; 20 ms as for the voice track.
    let mode = StreamMode::EventsShared { autoconvert: true, buffer_duration_hns: 200_000 };
    client.initialize_client(&format, &Direction::Capture, &mode).map_err(|e| format!("initialize: {e}"))?;
    let event = client.set_get_eventhandle().map_err(|e| e.to_string())?;
    let capture = client.get_audiocaptureclient().map_err(|e| e.to_string())?;
    client.start_stream().map_err(|e| e.to_string())?;
    let block = ring.channels as usize * 4;
    let mut raw: VecDeque<u8> = VecDeque::new();
    while !process.exited() {
        if event.wait_for_event(100).is_ok() && capture.read_from_device_to_deque(&mut raw).is_ok() && raw.len() >= block {
            let full = raw.len() - raw.len() % block;
            let samples: Vec<f32> = raw.drain(..full).collect::<Vec<u8>>().chunks_exact(4).map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]])).collect();
            clock.wrote((samples.len() / ring.channels as usize) as u64);
            ring.push_capture(&samples, false);
        }
        // A quiet app may send nothing: keep the track level with the clock.
        pad(ring, clock, started, SLACK);
    }
    let _ = client.stop_stream();
    Ok(())
}

#[link(name = "kernel32")]
extern "system" {
    fn OpenProcess(access: u32, inherit: i32, pid: u32) -> *mut c_void;
    fn WaitForSingleObject(handle: *mut c_void, millis: u32) -> u32;
    fn CloseHandle(handle: *mut c_void) -> i32;
}

/// A process handle that can only wait (SYNCHRONIZE).
struct Process(*mut c_void);

impl Process {
    fn open(pid: u32) -> Option<Self> {
        const SYNCHRONIZE: u32 = 0x0010_0000;
        // SAFETY: plain Win32 call; a null handle is checked before use.
        let handle = unsafe { OpenProcess(SYNCHRONIZE, 0, pid) };
        (!handle.is_null()).then_some(Self(handle))
    }

    fn exited(&self) -> bool {
        const WAIT_OBJECT_0: u32 = 0;
        // SAFETY: the handle is open for the life of `self`.
        unsafe { WaitForSingleObject(self.0, 0) == WAIT_OBJECT_0 }
    }
}

impl Drop for Process {
    fn drop(&mut self) {
        // SAFETY: closed exactly once.
        unsafe { CloseHandle(self.0) };
    }
}

#[cfg(test)]
#[path = "apps_tests.rs"]
mod tests;
