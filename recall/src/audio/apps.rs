//! App tracks: each `app` source (`sources.rs`; the older `RECALL_AUDIO_APPS`, a JSON
//! array of `{exe, label}`, still parses) is recorded as a track of its own, captured by
//! process loopback of the app's process tree (include-tree mode).
//!
//! Every chosen app gets its track for the whole recording, running or not, so a
//! clip's track count and order never depend on which apps were open. One scanner
//! thread lists processes for all closed apps (every 2 s; 5 s while a game is the
//! target). A watcher thread per app captures it while it runs and keeps the track
//! level with the wall clock (`Leveller`): silence while the app sends nothing, and
//! while it plays, drift corrected by at most 0.1%, a frame at a time, never in blocks.
//! An app that is the game itself stays silent: the Game track already holds it.
//!
//! Each track is saved with source `app:<exe>`, so the editor starts it out of the mix.
//! Not done: keeping a captured app out of Desktop audio. Process loopback can exclude
//! only one process tree per stream and ignores the chosen output device.

use std::collections::VecDeque;
use std::ffi::c_void;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde::Deserialize;
use wasapi::{Direction, SampleType, StreamMode, WaveFormat};

use super::sources::SourceSpec;
use super::voice::{snapshot_processes, tree_root, ProcEntry};
use super::TrackRing;

#[path = "apps_level.rs"]
mod level;
pub use level::Leveller;

pub const APPS_ENV: &str = "RECALL_AUDIO_APPS";
/// Same cap as the sources list.
const MAX_APPS: usize = super::sources::SOURCES_MAX;
/// How often closed apps are looked for: while recording the desktop, and while a game runs.
const RESCAN: Duration = Duration::from_secs(2);
const RESCAN_IN_GAME: Duration = Duration::from_secs(5);
/// An app that sends nothing for this long has no active stream: its track gets silence.
const IDLE: Duration = Duration::from_millis(200);

#[derive(Clone, Debug, PartialEq, Deserialize)]
pub struct AppSource {
    pub exe: String,
    #[serde(default)]
    pub label: String,
    /// The source's id ("" in the old list: `app-<n>`).
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub gain_db: f32,
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

/// One process list shared by every watcher. The scanner thread refreshes it only while
/// some app is closed (`closed` > 0).
struct Scanner {
    latest: Mutex<(u64, Arc<Vec<ProcEntry>>)>,
    closed: AtomicUsize,
}

impl Scanner {
    fn publish(&self, procs: Vec<ProcEntry>) {
        if let Ok(mut latest) = self.latest.lock() {
            *latest = (latest.0 + 1, Arc::new(procs));
        }
    }

    /// The newest list, when it is newer than `seen`.
    fn newer(&self, seen: u64) -> Option<(u64, Arc<Vec<ProcEntry>>)> {
        let latest = self.latest.lock().ok()?;
        (latest.0 > seen).then(|| (latest.0, Arc::clone(&latest.1)))
    }
}

/// One ring and one watcher thread per app, in list order, plus the shared scanner.
pub fn start(apps: &[AppSource], buffer_secs: u32, game_pid: Option<u32>, report: &mut impl FnMut(String)) -> Vec<Arc<TrackRing>> {
    if apps.is_empty() {
        return Vec::new();
    }
    let procs = snapshot_processes().unwrap_or_default();
    let game_exe = game_pid.and_then(|pid| procs.iter().find(|p| p.pid == pid)).map(|p| p.exe.to_lowercase());
    let scanner = Arc::new(Scanner { latest: Mutex::new((1, Arc::new(procs))), closed: AtomicUsize::new(0) });
    let interval = if game_pid.is_some() { RESCAN_IN_GAME } else { RESCAN };
    let scan = Arc::clone(&scanner);
    let _ = std::thread::Builder::new().name("recall-app-scan".into()).spawn(move || loop {
        std::thread::sleep(interval);
        if scan.closed.load(Ordering::Relaxed) > 0 {
            scan.publish(snapshot_processes().unwrap_or_default());
        }
    });
    let mut out = Vec::new();
    for (n, app) in apps.iter().enumerate() {
        let exe = app.exe.trim().to_owned();
        // Sources are `&'static str` across the engine; an engine run holds at most
        // MAX_APPS of them for its whole life, so leaking them is bounded.
        let source: &'static str = Box::leak(format!("{}{exe}", crate::edit_model::APP_SOURCE_PREFIX).into_boxed_str());
        let is_game = game_exe.as_deref().is_some_and(|g| level::same_exe(g, &exe));
        let id = if app.id.trim().is_empty() { format!("app-{}", n + 1) } else { app.id.trim().to_owned() };
        let spec = SourceSpec { id, kind: "app".into(), device: String::new(), exe: exe.clone(), name: app.track_name(), gain_db: app.gain_db };
        let ring = Arc::new(TrackRing::for_source(&spec, source, buffer_secs, format!("{exe} (not running)")));
        let name = ring.title();
        let (thread_ring, thread_scanner) = (Arc::clone(&ring), Arc::clone(&scanner));
        let thread_exe = exe.clone();
        let spawned = std::thread::Builder::new()
            .name(format!("recall-app-audio-{n}"))
            .spawn(move || if is_game { keep_silent(&thread_ring) } else { watch(&thread_ring, &thread_exe, &thread_scanner) });
        match spawned {
            Ok(_) if is_game => {
                report(format!("audio track '{name}' silent: {exe} is the game, already on the Game track"));
                out.push(ring);
            }
            Ok(_) => {
                report(format!("audio track '{name}' up: app {exe} (silent while it is closed)"));
                out.push(ring);
            }
            Err(e) => report(format!("audio track '{name}' UNAVAILABLE: {e}")),
        }
    }
    out
}

fn push_silence(ring: &TrackRing, frames: u64) {
    if frames > 0 {
        ring.push_capture(&vec![0.0; frames as usize * ring.channels as usize], false);
    }
}

fn set_endpoint(ring: &TrackRing, text: String) {
    if let Ok(mut endpoint) = ring.endpoint.lock() {
        *endpoint = text;
    }
}

/// The game's own exe: the track stays (track count is stable) but holds silence.
fn keep_silent(ring: &TrackRing) {
    let started = Instant::now();
    let mut level = Leveller::new(ring.rate);
    loop {
        std::thread::sleep(Duration::from_millis(100));
        push_silence(ring, level.idle(started.elapsed()));
    }
}

/// The app's watcher thread: capture while it runs, silence while it does not.
fn watch(ring: &TrackRing, exe: &str, scanner: &Scanner) {
    let _ = wasapi::initialize_mta();
    let started = Instant::now();
    let mut level = Leveller::new(ring.rate);
    let mut seen = 0;
    scanner.closed.fetch_add(1, Ordering::Relaxed);
    loop {
        if let Some((generation, procs)) = scanner.newer(seen) {
            seen = generation;
            if let Some(pid) = tree_root(&procs, exe) {
                scanner.closed.fetch_sub(1, Ordering::Relaxed);
                set_endpoint(ring, format!("{exe} pid {pid} (include tree)"));
                match capture_while_running(ring, pid, &mut level, started) {
                    Ok(()) => set_endpoint(ring, format!("{exe} (closed)")),
                    Err(e) => set_endpoint(ring, format!("{exe} pid {pid}: {e}")),
                }
                scanner.closed.fetch_add(1, Ordering::Relaxed);
            }
        }
        std::thread::sleep(Duration::from_millis(100));
        push_silence(ring, level.idle(started.elapsed()));
    }
}

/// Captures `pid`'s process tree until the process exits. Errors end the capture;
/// the watcher writes silence and looks for the app again.
fn capture_while_running(ring: &TrackRing, pid: u32, level: &mut Leveller, started: Instant) -> Result<(), String> {
    let process = Process::open(pid).ok_or_else(|| format!("open process: {}", std::io::Error::last_os_error()))?;
    let mut client = wasapi::AudioClient::new_application_loopback_client(pid, true).map_err(|e| format!("process loopback: {e}"))?;
    let format = WaveFormat::new(32, 32, &SampleType::Float, ring.rate as usize, ring.channels as usize, None);
    // Process-loopback clients have no device period; 20 ms as for the voice track.
    let mode = StreamMode::EventsShared { autoconvert: true, buffer_duration_hns: 200_000 };
    client.initialize_client(&format, &Direction::Capture, &mode).map_err(|e| format!("initialize: {e}"))?;
    let event = client.set_get_eventhandle().map_err(|e| e.to_string())?;
    let capture = client.get_audiocaptureclient().map_err(|e| e.to_string())?;
    client.start_stream().map_err(|e| e.to_string())?;
    let channels = ring.channels as usize;
    let block = channels * 4;
    let mut raw: VecDeque<u8> = VecDeque::new();
    let mut last_sound = Instant::now();
    while !process.exited() {
        if event.wait_for_event(100).is_ok() && capture.read_from_device_to_deque(&mut raw).is_ok() && raw.len() >= block {
            let full = raw.len() - raw.len() % block;
            let samples: Vec<f32> = raw.drain(..full).collect::<Vec<u8>>().chunks_exact(4).map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]])).collect();
            ring.push_capture(&level.live(started.elapsed(), &samples, channels), false);
            last_sound = Instant::now();
        } else if last_sound.elapsed() >= IDLE {
            // No active stream: the app is quiet, so silence up to the clock cuts nothing.
            push_silence(ring, level.idle(started.elapsed()));
        }
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
