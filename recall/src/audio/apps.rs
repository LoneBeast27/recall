//! App tracks: each `app` source (`sources.rs`; the older `RECALL_AUDIO_APPS`, a JSON
//! array of `{exe, label}`, still parses) is recorded as a track of its own, captured by
//! process loopback of the app's process tree (include-tree mode).
//!
//! Every chosen app gets its track for the whole recording, running or not, so a
//! clip's track count and order never depend on which apps were open. One scanner
//! thread lists processes for all closed apps when Windows says a process started
//! audio (`apps_sessions.rs`). A watcher thread per app captures it while it runs and
//! keeps the track level with the wall clock (`Leveller`): silence while the app sends
//! nothing, and while it plays, drift corrected by at most 0.1%, a frame at a time,
//! never in blocks. An app that is the game itself stays silent: the Game track already
//! holds it. The watchers sleep on their track's wake (`wake.rs`): the silence is written
//! when a save needs it, not on a timer.
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

use super::capture_loop::Notices;
use super::sources::SourceSpec;
use super::stream::Event;
use super::silence::{QuietMark, QUIET_FILL};
use super::wake::{TrackWake, Waker, Woke};
use super::voice::{snapshot_processes, tree_root, ProcEntry};
use super::TrackRing;

#[path = "apps_level.rs"]
mod level;
#[path = "apps_sessions.rs"]
mod sessions;
#[path = "apps_presence.rs"]
mod presence;
use presence::Slot;
pub use level::Leveller;

pub const APPS_ENV: &str = "RECALL_AUDIO_APPS";
/// Same cap as the sources list.
const MAX_APPS: usize = super::sources::SOURCES_MAX;
/// After a capture ends: list processes at once, then this often for [`RELAUNCH_WINDOW`].
/// timer-ok: a relaunch that reuses an existing session sends no notice.
const RELAUNCH_EVERY: Duration = Duration::from_secs(2);
const RELAUNCH_WINDOW: Duration = Duration::from_secs(30);
/// How often closed apps are looked for when Windows gives no session notices: while
/// recording the desktop, and while a game runs. timer-ok: only without notices.
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
    /// The watchers' wakes: a new list wakes them.
    watchers: Mutex<Vec<Waker>>,
    /// The scanner thread's own wake (session notices, a capture that ended).
    wake: TrackWake,
    /// After a capture ends, the scanner lists every [`RELAUNCH_EVERY`] until then: a relaunched
    /// app that already has its audio session announces nothing new.
    burst_until: Mutex<Option<Instant>>,
    /// The chosen apps, for the running label (`apps_presence.rs`).
    slots: Mutex<Vec<Arc<Slot>>>,
}

impl Scanner {
    fn publish(&self, procs: Vec<ProcEntry>) {
        if let Ok(mut latest) = self.latest.lock() {
            *latest = (latest.0 + 1, Arc::new(procs));
        }
        self.watchers.lock().unwrap_or_else(|p| p.into_inner()).iter().for_each(Waker::wake);
    }

    /// A capture just ended: look at once, then every [`RELAUNCH_EVERY`] for a while.
    fn kick(&self) {
        *self.burst_until.lock().unwrap_or_else(|p| p.into_inner()) = Some(Instant::now() + RELAUNCH_WINDOW);
        self.wake.wake();
    }

    fn bursting(&self, now: Instant) -> bool {
        self.burst_until.lock().unwrap_or_else(|p| p.into_inner()).is_some_and(|until| now < until)
    }

    /// The newest list, when it is newer than `seen`.
    fn newer(&self, seen: u64) -> Option<(u64, Arc<Vec<ProcEntry>>)> {
        let latest = self.latest.lock().ok()?;
        (latest.0 > seen).then(|| (latest.0, Arc::clone(&latest.1)))
    }
}

/// One ring and one watcher thread per app, in list order, plus the shared scanner.
pub fn start(apps: &[AppSource], buffer_secs: u32, game_pid: Option<u32>, hold: bool, report: &mut impl FnMut(String)) -> Vec<Arc<TrackRing>> {
    if apps.is_empty() {
        return Vec::new();
    }
    let procs = snapshot_processes().unwrap_or_default();
    let game_exe = game_pid.and_then(|pid| procs.iter().find(|p| p.pid == pid)).map(|p| p.exe.to_lowercase());
    let scanner = Arc::new(Scanner {
        latest: Mutex::new((1, Arc::new(procs))),
        closed: AtomicUsize::new(0),
        watchers: Mutex::new(Vec::new()),
        wake: TrackWake::default(),
        burst_until: Mutex::new(None),
        slots: Mutex::new(Vec::new()),
    });
    let fallback = if game_pid.is_some() { RESCAN_IN_GAME } else { RESCAN };
    let scan = Arc::clone(&scanner);
    let _ = std::thread::Builder::new().name("recall-app-scan".into()).spawn(move || scan_thread(&scan, fallback));
    let mut out = Vec::new();
    for (n, app) in apps.iter().enumerate() {
        let exe = app.exe.trim().to_owned();
        // Sources are `&'static str` across the engine; an engine run holds at most
        // MAX_APPS of them for its whole life, so leaking them is bounded.
        let source: &'static str = Box::leak(format!("{}{exe}", crate::edit_model::APP_SOURCE_PREFIX).into_boxed_str());
        let is_game = game_exe.as_deref().is_some_and(|g| level::same_exe(g, &exe));
        let id = if app.id.trim().is_empty() { format!("app-{}", n + 1) } else { app.id.trim().to_owned() };
        let spec = SourceSpec { communications: false, device_id: String::new(), id, kind: "app".into(), device: String::new(), exe: exe.clone(), name: app.track_name(), gain_db: app.gain_db, mic_mute: Some(false) };
        let ring = Arc::new(TrackRing::for_source(&spec, source, buffer_secs, format!("{exe} (not running)")));
        if hold {
            ring.hold_startup(); // "Hold everything" until the controller's sync (the cut-down)
        }
        let name = ring.title();
        let (thread_ring, thread_scanner) = (Arc::clone(&ring), Arc::clone(&scanner));
        let slot = Arc::new(Slot::new(Arc::clone(&ring), exe.clone()));
        if !is_game {
            scanner.slots.lock().unwrap_or_else(|p| p.into_inner()).push(Arc::clone(&slot));
        }
        let spawned = std::thread::Builder::new()
            .name(format!("recall-app-audio-{n}"))
            .spawn(move || if is_game { keep_silent(&thread_ring) } else { watch(&thread_ring, &slot, &thread_scanner) });
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

/// Silence up to the clock in one block, and the quiet mark a snapshot pads on from.
fn quiet_fill(ring: &TrackRing, level: &mut Leveller, started: Instant) {
    let frames = level.idle(started.elapsed());
    ring.push_silence_frames(frames, Some(QuietMark { origin: started, written: level.written() }));
}

fn set_endpoint(ring: &TrackRing, text: String) {
    if let Ok(mut endpoint) = ring.endpoint.lock() {
        *endpoint = text;
    }
}

/// The scanner: a new process list for the watchers of closed apps whenever a process starts
/// audio (or an endpoint comes, which is then watched too), after a capture ends, and on the
/// safety rescan. Without session notices it lists every `fallback`, as before.
fn scan_thread(scan: &Scanner, fallback: Duration) {
    let _ = wasapi::initialize_mta();
    let sessions = sessions::Sessions::register(&scan.wake);
    let endpoints = super::notify::EndpointNotices::register(wasapi::Direction::Render, false, scan.wake.waker());
    let notices = match (&sessions, &endpoints) {
        (Ok(_), Ok(_)) => true,
        (Err(e), _) | (_, Err(e)) => {
            eprintln!("[recall] app audio: no session notices ({e}); looking for closed apps every {} s", fallback.as_secs());
            false
        }
    };
    let mut sessions = sessions.ok();
    let mut safety_at = Instant::now() + sessions::SAFETY_RESCAN;
    loop {
        let closed = scan.closed.load(Ordering::Relaxed) > 0;
        let interval = if !notices {
            fallback
        } else if closed {
            presence::PRESENCE_EVERY.min(RELAUNCH_EVERY)
        } else {
            sessions::SAFETY_RESCAN
        };
        let woke = scan.wake.wait(&[], Some(interval));
        if let (Some(s), Ok(n)) = (sessions.as_mut(), &endpoints) {
            if n.take_devices_changed() {
                s.refresh();
            }
        }
        if scan.closed.load(Ordering::Relaxed) == 0 {
            continue;
        }
        let now = Instant::now();
        let procs = snapshot_processes().unwrap_or_default();
        presence::update_labels(&scan.slots.lock().unwrap_or_else(|p| p.into_inner()), &procs);
        // Capture follows sessions: a notice, a capture that just ended, or the safety rescan.
        if !notices || woke == Woke::Wake || scan.bursting(now) || now >= safety_at {
            safety_at = now + sessions::SAFETY_RESCAN;
            scan.publish(procs);
        }
    }
}

/// The game's own exe: the track stays (track count is stable) but holds silence, written at
/// least once a second (`silence.rs`; a snapshot pads the rest).
fn keep_silent(ring: &TrackRing) {
    let started = Instant::now();
    let mut level = Leveller::new(ring.rate).with_cap(ring.capacity_frames());
    loop {
        quiet_fill(ring, &mut level, started);
        ring.live.wake.wait(&[], Some(QUIET_FILL));
    }
}

/// The app's watcher thread: capture while it runs, silence while it does not.
fn watch(ring: &TrackRing, slot: &Slot, scanner: &Scanner) {
    let _ = wasapi::initialize_mta();
    super::stream::join_mmcss_audio(ring.name);
    let started = Instant::now();
    let mut level = Leveller::new(ring.rate).with_cap(ring.capacity_frames());
    let exe = slot.exe.as_str();
    let mut seen = 0;
    scanner.closed.fetch_add(1, Ordering::Relaxed);
    scanner.watchers.lock().unwrap_or_else(|p| p.into_inner()).push(ring.live.wake.waker());
    loop {
        if let Some((generation, procs)) = scanner.newer(seen) {
            seen = generation;
            if let Some(pid) = tree_root(&procs, exe) {
                scanner.closed.fetch_sub(1, Ordering::Relaxed);
                slot.closed.store(false, Ordering::SeqCst);
                set_endpoint(ring, format!("{exe} pid {pid} (include tree)"));
                match capture_while_running(ring, pid, &mut level, started) {
                    Ok(()) => set_endpoint(ring, format!("{exe} (closed)")),
                    Err(e) => set_endpoint(ring, format!("{exe} pid {pid}: {e}")),
                }
                slot.forget();
                slot.closed.store(true, Ordering::SeqCst);
                scanner.closed.fetch_add(1, Ordering::Relaxed);
                scanner.kick(); // a relaunch may reuse its session: look again now and for a while
            }
        }
        // Out of live sound, up to the clock; then until a new process list or a second.
        quiet_fill(ring, &mut level, started);
        ring.live.wake.wait(&[], Some(QUIET_FILL));
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
    // Closed on drop (a bare wasapi `Handle` leaked one per app start), after the client.
    let event = Event(client.set_get_eventhandle().map_err(|e| e.to_string())?);
    let result = capture_events(ring, &mut client, &event, &process, level, started);
    drop(client);
    drop(event);
    result
}

/// Capture `client`'s packets until `process` exits (`capture_while_running`).
fn capture_events(ring: &TrackRing, client: &mut wasapi::AudioClient, event: &Event, process: &Process, level: &mut Leveller, started: Instant) -> Result<(), String> {
    let capture = client.get_audiocaptureclient().map_err(|e| e.to_string())?;
    client.start_stream().map_err(|e| e.to_string())?;
    let channels = ring.channels as usize;
    let block = channels * 4;
    let mut raw: VecDeque<u8> = VecDeque::new();
    // Kept across packets: no allocation per packet once they fit (merge-gate P3).
    let (mut samples, mut leveled) = (Vec::new(), Vec::new());
    let mut last_sound = Instant::now();
    // No live sound since the last packet: the leveller is idle (true at the start: the watcher
    // left it idle), so the next packet first gets the silence owed before it.
    let mut quiet = true;
    let wake = &ring.live.wake;
    loop {
        // The process first: it ends the capture as soon as it exits. Quiet: a fill a second.
        let timeout = if quiet { QUIET_FILL } else { IDLE.saturating_sub(last_sound.elapsed()) };
        match wake.wait(&[process.handle(), event.raw()], Some(timeout)) {
            Woke::Handle(0) => break,
            Woke::Handle(1) if capture.read_from_device_to_deque(&mut raw).is_ok() && raw.len() >= block => {
                super::packet::decode(&mut raw, block, &mut samples);
                let elapsed = started.elapsed();
                if quiet {
                    super::capture_loop::silence_before(ring, level, elapsed, samples.len() / channels);
                }
                level.live_into(elapsed, &samples, channels, &mut leveled);
                ring.push_app_capture(&leveled); // a retired app source stores silence at once
                last_sound = Instant::now();
                quiet = false;
                continue;
            }
            _ => {}
        }
        if quiet || last_sound.elapsed() >= IDLE {
            // No active stream: the app is quiet, so silence up to the clock cuts nothing.
            quiet_fill(ring, level, started);
            quiet = true;
        }
    }
    let _ = client.stop_stream();
    Ok(())
}

#[link(name = "kernel32")]
extern "system" {
    fn OpenProcess(access: u32, inherit: i32, pid: u32) -> *mut c_void;
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

    /// The handle a wait ends on when the process exits (open for the life of `self`).
    fn handle(&self) -> windows::Win32::Foundation::HANDLE {
        windows::Win32::Foundation::HANDLE(self.0)
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
