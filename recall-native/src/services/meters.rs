//! Level meters for the Audio page's sources, one per source: an output device by loopback,
//! an input by capture, an app or the recorded game by process loopback. Levels are measured
//! before the source's gain (the page adds it). Ported from the hub's `meters.rs`; missing
//! devices and closed apps are retried.
use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use wasapi::{AudioClient, Device, DeviceCollection, Direction, SampleType, StreamMode, WaveFormat};

use crate::services::audio_sources::{Kind, Source};

#[derive(Debug, Clone, PartialEq)]
pub enum MeterState {
    Live,
    Missing,
    Error(String),
}

#[derive(Debug, Clone, PartialEq)]
pub struct MeterReading {
    /// The source's id.
    pub label: String,
    pub rms_db: f32,
    pub peak_db: f32,
    pub state: MeterState,
}

/// What one meter listens to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MeterTarget {
    /// An endpoint ("" = the Windows default): loopback of an output, or an input.
    Device { render: bool, device: String },
    /// The largest running process tree of an exe.
    App { exe: String },
    /// One process tree (the recorded game).
    Process { pid: u32 },
}

/// One meter: the source's id and what it listens to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MeterSpec {
    pub label: String,
    pub target: MeterTarget,
}

/// The meter for a source, the way the recorder captures it: the game's process while a game
/// is recorded (`game_pid`), else its output device.
pub fn spec_for(source: &Source, game_pid: Option<u32>) -> MeterSpec {
    let device = source.device.trim().to_owned();
    let target = match source.kind {
        Kind::Game => match game_pid {
            Some(pid) => MeterTarget::Process { pid },
            None => MeterTarget::Device { render: true, device },
        },
        Kind::Output => MeterTarget::Device { render: true, device },
        Kind::Input => MeterTarget::Device { render: false, device },
        Kind::App => MeterTarget::App { exe: source.exe.trim().to_owned() },
    };
    MeterSpec { label: source.id.clone(), target }
}

const FLOOR_DB: f32 = -90.0;
const RETRY: Duration = Duration::from_secs(3);
const UPDATE: Duration = Duration::from_millis(16);

pub struct Meters {
    shared: Arc<Shared>,
}

struct Shared {
    specs: Vec<MeterSpec>,
    readings: Mutex<Vec<MeterReading>>,
    revision: AtomicU64,
    stop: AtomicBool,
}

impl Shared {
    fn new(specs: Vec<MeterSpec>) -> Self {
        let readings = specs.iter().map(|spec| reading(&spec.label, FLOOR_DB, FLOOR_DB, MeterState::Missing)).collect();
        Self { specs, readings: Mutex::new(readings), revision: AtomicU64::new(0), stop: AtomicBool::new(false) }
    }

    /// Stores a reading; bumps the revision only when something visible changed.
    fn set(&self, index: usize, rms_db: f32, peak_db: f32, state: MeterState) {
        let next = reading(&self.specs[index].label, round_tenth(rms_db), round_tenth(peak_db), state);
        let mut readings = match self.readings.lock() {
            Ok(readings) => readings,
            Err(poisoned) => poisoned.into_inner(),
        };
        if readings[index] != next {
            readings[index] = next;
            self.revision.fetch_add(1, Ordering::AcqRel);
        }
    }

    fn stopped(&self) -> bool {
        self.stop.load(Ordering::Acquire)
    }
}

fn reading(label: &str, rms_db: f32, peak_db: f32, state: MeterState) -> MeterReading {
    MeterReading { label: label.into(), rms_db, peak_db, state }
}

impl Meters {
    /// Starts one thread per meter. Never blocks. A test instance (`RECALL_NATIVE_OFFSCREEN`)
    /// starts none: it opens no real mic, and every meter reads missing.
    pub fn spawn(specs: Vec<MeterSpec>) -> Self {
        let shared = Arc::new(Shared::new(specs));
        let meters = if std::env::var_os("RECALL_NATIVE_OFFSCREEN").is_some() { 0 } else { shared.specs.len() };
        for index in 0..meters {
            let worker = shared.clone();
            let name = format!("vu-{}", shared.specs[index].label);
            if let Err(error) = std::thread::Builder::new().name(name).spawn(move || meter_thread(&worker, index)) {
                shared.set(index, FLOOR_DB, FLOOR_DB, MeterState::Error(format!("meter thread: {error}")));
            }
        }
        Self { shared }
    }
    /// What these meters listen to (a settings change replaces the meters).
    pub fn specs(&self) -> &[MeterSpec] {
        &self.shared.specs
    }
    /// Latest readings in display order (one per spec).
    pub fn snapshot(&self) -> Vec<MeterReading> {
        match self.shared.readings.lock() {
            Ok(readings) => readings.clone(),
            Err(poisoned) => poisoned.into_inner().clone(),
        }
    }
    pub fn revision(&self) -> u64 {
        self.shared.revision.load(Ordering::Acquire)
    }
}

impl Drop for Meters {
    fn drop(&mut self) {
        self.shared.stop.store(true, Ordering::Release);
    }
}

/// A level measured before the gain, as it sounds after it: silence stays silence, anything
/// else moves by the gain and stays in -90..+12 dB. The page applies this to the bar, the
/// peak-hold line and the number alike, so all three follow the fader together.
pub fn with_gain(db: f32, gain_db: f32) -> f32 {
    if db <= FLOOR_DB { FLOOR_DB } else { (db + gain_db).clamp(FLOOR_DB, 12.0) }
}

/// Maps dB to a 0..1 bar fraction over a -60..0 dB scale.
pub fn bar_fraction(db: f32) -> f32 {
    ((db + 60.0) / 60.0).clamp(0.0, 1.0)
}

/// Find → capture until error or device loss → report → retry every 3 s. Never exits
/// on a missing device (the hub's thread returned for good).
fn meter_thread(shared: &Shared, index: usize) {
    let _ = wasapi::initialize_mta();
    let spec = shared.specs[index].clone();
    while !shared.stopped() {
        let state = match open(&spec.target) {
            Ok(Some(opened)) => match capture(shared, index, opened) {
                // The Windows default moved: meter the new one at once, as the recorder does.
                Ok(Ended::DefaultMoved) => continue,
                Ok(Ended::Closed) => MeterState::Missing,
                Err(error) => MeterState::Error(error),
            },
            Ok(None) => MeterState::Missing,
            Err(error) => MeterState::Error(error),
        };
        if shared.stopped() {
            break;
        }
        shared.set(index, FLOOR_DB, FLOOR_DB, state);
        let resume = Instant::now() + RETRY;
        while Instant::now() < resume && !shared.stopped() {
            std::thread::sleep(Duration::from_millis(200));
        }
    }
}

/// The endpoint the recorder opens for `name` (`recall` `audio::pick_device`), and for a source
/// with no device the Windows default's id (the meter follows it when it moves). A saved device
/// that is not connected is metered as missing: the recorder records silence for it, so a live
/// meter of the default there would claim a recording that is not happening.
fn find_device(direction: &Direction, name: &str) -> Result<Option<(Device, Option<String>)>, String> {
    let collection = DeviceCollection::new(direction).map_err(|error| error.to_string())?;
    let count = collection.get_nbr_devices().map_err(|error| error.to_string())?;
    let devices: Vec<Device> = (0..count).filter_map(|i| collection.get_device_at_index(i).ok()).collect();
    let names: Vec<String> = devices.iter().map(|d| d.get_friendlyname().unwrap_or_default()).collect();
    match meter_pick(&names, name) {
        Some(DevicePick::Named(i)) => Ok(devices.into_iter().nth(i).map(|d| (d, None))),
        Some(DevicePick::Default) => Ok(wasapi::get_default_device(direction).ok().map(|d| {
            let id = d.get_id().ok();
            (d, id)
        })),
        None => Ok(None),
    }
}

/// The id of the Windows default endpoint for `direction` right now.
fn default_id(direction: &Direction) -> Option<String> {
    wasapi::get_default_device(direction).ok()?.get_id().ok()
}

/// What a device meter opens.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DevicePick {
    /// The endpoint at this index of the names.
    Named(usize),
    /// The Windows default.
    Default,
}

/// The endpoint a device meter opens for the saved device `want`, from the connected endpoints'
/// `names`, the way the recorder picks it (`recall` `audio::choose_endpoint`).
pub fn meter_pick(names: &[String], want: &str) -> Option<DevicePick> {
    if want.trim().is_empty() {
        return Some(DevicePick::Default);
    }
    match_endpoint(names, want).map(DevicePick::Named)
}

/// A default-device meter should reopen: the Windows default it opened (`opened`) is no longer
/// the default (`now`). No default right now is not a move: the stream reports the loss itself.
pub fn default_moved(opened: Option<&str>, now: Option<&str>) -> bool {
    matches!((opened, now), (Some(opened), Some(now)) if opened != now)
}

/// Index of the endpoint `want` names: the one endpoint with exactly that friendly name; else
/// the one endpoint whose name equals `want`'s once both drop a leading "<digits>- " Windows
/// index. Case included, no word matching; two or more exact matches, or two or more such
/// endpoints, match none (the track waits, never a sibling). A copy of the recorder's
/// `match_endpoint` (`recall` `audio/endpoints.rs`); both crates test the same table.
pub fn match_endpoint(names: &[String], want: &str) -> Option<usize> {
    let want = want.trim();
    if want.is_empty() {
        return None;
    }
    let mut exact = names.iter().enumerate().filter(|(_, n)| n.trim() == want).map(|(i, _)| i);
    if let Some(first) = exact.next() {
        return exact.next().is_none().then_some(first);
    }
    let want = strip_index(want);
    let mut hits = names.iter().enumerate().filter(|(_, n)| strip_index(n) == want).map(|(i, _)| i);
    let first = hits.next()?;
    hits.next().is_none().then_some(first)
}

/// How many endpoints equal `want` once both drop a leading "<digits>- " index: two or more
/// with no exact match is why `match_endpoint` opens none ("3- X" saved, "4- X" and "5- X" there).
pub fn stripped_hits(names: &[String], want: &str) -> usize {
    let want = strip_index(want);
    names.iter().filter(|n| strip_index(n) == want).count()
}

/// A saved endpoint id the recorder trusts: 1 to 256 characters once trimmed, no control
/// character; anything else counts as no id (R6-deep-audio-5). A copy of the recorder's `id_ok`
/// (`recall` `audio/endpoints.rs`).
pub fn id_ok(id: &str) -> bool {
    let id = id.trim();
    !id.is_empty() && id.chars().count() <= 256 && !id.chars().any(char::is_control)
}

/// The endpoint the recorder opened (its id `opened_id`) is the source's own saved device
/// (`want`, the id saved with it): a well-formed saved id equal to the opened one. The id alone
/// decides; no id = not own. Only then may a line-in skip Alt+M (R6-deep-audio-1/2). A copy of
/// the recorder's `opened_own` (`recall` `audio/endpoints.rs`); both crates test the same table.
pub fn opened_own(opened_id: Option<&str>, want: &str, want_id: &str) -> bool {
    let want_id = want_id.trim();
    !want.trim().is_empty() && id_ok(want_id) && opened_id.map(str::trim) == Some(want_id)
}

/// `name` without a leading "<digits>- " Windows endpoint index; nothing else is dropped.
fn strip_index(name: &str) -> &str {
    let name = name.trim();
    let digits = name.len() - name.trim_start_matches(|d: char| d.is_ascii_digit()).len();
    match name[digits..].strip_prefix("- ") {
        Some(rest) if digits > 0 => rest.trim_start(),
        _ => name,
    }
}

/// One opened meter client.
struct Opened {
    client: AudioClient,
    /// Event period (hns).
    period: i64,
    /// A captured process (the recorded game, an app).
    pid: Option<u32>,
    /// A source with no device: the direction and the Windows default's id it opened.
    default: Option<(Direction, String)>,
}

/// How a capture ended without an error.
enum Ended {
    /// Stopped, or the captured process exited.
    Closed,
    /// The Windows default moved to another endpoint.
    DefaultMoved,
}

/// An audio client for a target; `None` while the device is missing or the app is closed.
fn open(target: &MeterTarget) -> Result<Option<Opened>, String> {
    let text = |error: wasapi::WasapiError| error.to_string();
    let process = |pid: u32| -> Result<Option<Opened>, String> {
        // Process-loopback clients have no device period; 20 ms as the recorder uses.
        Ok(Some(Opened { client: AudioClient::new_application_loopback_client(pid, true).map_err(text)?, period: 200_000, pid: Some(pid), default: None }))
    };
    match target {
        MeterTarget::Device { render, device } => {
            let direction = if *render { Direction::Render } else { Direction::Capture };
            let Some((device, default)) = find_device(&direction, device)? else { return Ok(None) };
            let client = device.get_iaudioclient().map_err(text)?;
            let (_, min_period) = client.get_device_period().map_err(text)?;
            Ok(Some(Opened { client, period: min_period, pid: None, default: default.map(|id| (direction, id)) }))
        }
        MeterTarget::App { exe } => match crate::services::process_tree::tree_root(exe) {
            Some(pid) => process(pid),
            None => Ok(None),
        },
        MeterTarget::Process { pid } => process(*pid),
    }
}

/// wasapi's event handle, closed on drop: wasapi 0.19's `Handle` has no `Drop`, so every meter
/// reopen leaked one. Drop it after the client that signals it.
struct Event(wasapi::Handle);

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

/// How often a default-device meter checks that the default has not moved.
const DEFAULT_CHECK: Duration = Duration::from_secs(1);

/// Streams levels at ~10 Hz until asked to stop, a captured process exits, or the Windows
/// default it meters moves; device loss surfaces as an error from the read or the padding probe.
fn capture(shared: &Shared, index: usize, opened: Opened) -> Result<Ended, String> {
    let Opened { mut client, period, pid, default } = opened;
    let format = WaveFormat::new(32, 32, &SampleType::Float, 48000, 2, None);
    client
        .initialize_client(&format, &Direction::Capture, &StreamMode::EventsShared { autoconvert: true, buffer_duration_hns: period })
        .map_err(|error| error.to_string())?;
    let event = Event(client.set_get_eventhandle().map_err(|error| error.to_string())?);
    let ended = levels_until_end(shared, index, &mut client, &event, pid, default.as_ref());
    drop(client);
    drop(event);
    ended
}

/// The capture loop of [`capture`].
fn levels_until_end(shared: &Shared, index: usize, client: &mut AudioClient, event: &Event, pid: Option<u32>, default: Option<&(Direction, String)>) -> Result<Ended, String> {
    let text = |error: wasapi::WasapiError| error.to_string();
    let process = pid.and_then(crate::services::process_tree::Waitable::open);
    let capture = client.get_audiocaptureclient().map_err(text)?;
    client.start_stream().map_err(text)?;
    let mut raw: VecDeque<u8> = VecDeque::new();
    let mut levels = Levels::default();
    let mut last = Instant::now();
    let mut checked = Instant::now();
    while !shared.stopped() {
        if let Some((direction, opened)) = default.filter(|_| checked.elapsed() >= DEFAULT_CHECK) {
            checked = Instant::now();
            if default_moved(Some(opened), default_id(direction).as_deref()) {
                let _ = client.stop_stream();
                return Ok(Ended::DefaultMoved);
            }
        }
        // Loopback streams raise no events while nothing renders: a timeout is silence.
        if event.0.wait_for_event(200).is_ok() {
            capture.read_from_device_to_deque(&mut raw).map_err(|error| format!("read failed: {error}"))?;
            let full = raw.len() - raw.len() % 4;
            levels.push_bytes(&raw.make_contiguous()[..full]);
            raw.drain(..full);
        } else {
            client.get_current_padding().map_err(|error| format!("device lost: {error}"))?;
            if process.as_ref().is_some_and(|p| p.exited()) {
                break;
            }
        }
        if last.elapsed() >= UPDATE {
            let (rms_db, peak_db) = levels.take();
            shared.set(index, rms_db, peak_db, MeterState::Live);
            last = Instant::now();
        }
    }
    let _ = client.stop_stream();
    Ok(Ended::Closed)
}

/// RMS + peak accumulator over interleaved f32 samples (all channels together).
#[derive(Debug, Default)]
struct Levels {
    sum_sq: f64,
    peak: f32,
    count: usize,
}

impl Levels {
    fn push_bytes(&mut self, bytes: &[u8]) {
        for chunk in bytes.chunks_exact(4) {
            let sample = f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]);
            if sample.is_finite() {
                self.sum_sq += f64::from(sample) * f64::from(sample);
                self.peak = self.peak.max(sample.abs());
                self.count += 1;
            }
        }
    }

    /// (rms dBFS, peak dBFS) since the last take; resets the window.
    fn take(&mut self) -> (f32, f32) {
        let rms = if self.count > 0 { (self.sum_sq / self.count as f64).sqrt() as f32 } else { 0.0 };
        let result = (db(rms), db(self.peak));
        *self = Self::default();
        result
    }
}

fn db(value: f32) -> f32 {
    if value <= 1e-9 { FLOOR_DB } else { (20.0 * value.log10()).max(FLOOR_DB) }
}

fn round_tenth(value: f32) -> f32 {
    (value * 10.0).round() / 10.0
}

#[cfg(test)]
#[path = "meters_tests.rs"]
mod tests;
