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
            Ok(Some((client, period, pid))) => match capture(shared, index, client, period, pid) {
                Ok(()) => MeterState::Missing,
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

/// The endpoint the recorder opens for `name` (`recall` `audio.rs` `pick_device`): exact name
/// first, then a whole-word match, else the Windows default. A saved device that is gone
/// records from the default, so the meter shows the default too; the strip says "Not connected".
fn find_device(is_render: bool, name: &str) -> Result<Option<Device>, String> {
    let direction = if is_render { Direction::Render } else { Direction::Capture };
    if !name.is_empty() {
        let collection = DeviceCollection::new(&direction).map_err(|error| error.to_string())?;
        let count = collection.get_nbr_devices().map_err(|error| error.to_string())?;
        let devices: Vec<Device> = (0..count).filter_map(|i| collection.get_device_at_index(i).ok()).collect();
        let names: Vec<String> = devices.iter().map(|d| d.get_friendlyname().unwrap_or_default()).collect();
        if let Some(device) = match_endpoint(&names, name).and_then(|i| devices.into_iter().nth(i)) {
            return Ok(Some(device));
        }
    }
    Ok(wasapi::get_default_device(&direction).ok())
}

/// Index of the endpoint `want` names: exact (case-insensitive) first, then the first whole-word
/// match. A copy of the recorder's `match_endpoint`; both crates test the same table.
pub fn match_endpoint(names: &[String], want: &str) -> Option<usize> {
    let want = want.trim();
    if want.is_empty() {
        return None;
    }
    let want_lower = want.to_lowercase();
    names.iter().position(|n| n.trim().to_lowercase() == want_lower).or_else(|| names.iter().position(|n| name_matches(n, want)))
}

/// An audio client for a target, with its event period and, for a process, its pid; `None`
/// while the device is missing or the app is closed.
fn open(target: &MeterTarget) -> Result<Option<(AudioClient, i64, Option<u32>)>, String> {
    let text = |error: wasapi::WasapiError| error.to_string();
    let process = |pid: u32| -> Result<Option<(AudioClient, i64, Option<u32>)>, String> {
        // Process-loopback clients have no device period; 20 ms as the recorder uses.
        Ok(Some((AudioClient::new_application_loopback_client(pid, true).map_err(text)?, 200_000, Some(pid))))
    };
    match target {
        MeterTarget::Device { render, device } => {
            let Some(device) = find_device(*render, device)? else { return Ok(None) };
            let client = device.get_iaudioclient().map_err(text)?;
            let (_, min_period) = client.get_device_period().map_err(text)?;
            Ok(Some((client, min_period, None)))
        }
        MeterTarget::App { exe } => match crate::services::process_tree::tree_root(exe) {
            Some(pid) => process(pid),
            None => Ok(None),
        },
        MeterTarget::Process { pid } => process(*pid),
    }
}

/// Streams levels at ~10 Hz. Returns Ok(()) when asked to stop or when a captured process
/// exits; device loss surfaces as an error from the read or the padding probe.
fn capture(shared: &Shared, index: usize, mut client: AudioClient, period: i64, pid: Option<u32>) -> Result<(), String> {
    let text = |error: wasapi::WasapiError| error.to_string();
    let format = WaveFormat::new(32, 32, &SampleType::Float, 48000, 2, None);
    client
        .initialize_client(&format, &Direction::Capture, &StreamMode::EventsShared { autoconvert: true, buffer_duration_hns: period })
        .map_err(text)?;
    let process = pid.and_then(crate::services::process_tree::Waitable::open);
    let event = client.set_get_eventhandle().map_err(text)?;
    let capture = client.get_audiocaptureclient().map_err(text)?;
    client.start_stream().map_err(text)?;
    let mut raw: VecDeque<u8> = VecDeque::new();
    let mut levels = Levels::default();
    let mut last = Instant::now();
    while !shared.stopped() {
        // Loopback streams raise no events while nothing renders: a timeout is silence.
        if event.wait_for_event(200).is_ok() {
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
    Ok(())
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

/// Lowercase, ø→o (so a name typed without the ø still matches), split into words.
fn words(text: &str) -> Vec<String> {
    let folded: String = text.to_lowercase().chars().map(|c| if c == 'ø' { 'o' } else { c }).collect();
    folded.split(|c: char| !c.is_alphanumeric()).filter(|word| !word.is_empty()).map(str::to_owned).collect()
}

/// Every token must appear as a whole word of the friendly name.
fn name_matches(name: &str, tokens: &str) -> bool {
    let name_words = words(name);
    let want = words(tokens);
    !want.is_empty() && want.iter().all(|token| name_words.contains(token))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn source(kind: Kind, device: &str, exe: &str) -> Source {
        Source { id: format!("{}-1", kind.token()), kind, device: device.into(), exe: exe.into(), name: "S".into(), enabled: true, gain_db: 0.0 }
    }

    #[test]
    fn each_source_is_metered_the_way_the_recorder_captures_it() {
        let device = |render: bool, d: &str| MeterTarget::Device { render, device: d.into() };
        assert_eq!(spec_for(&source(Kind::Input, " Microphone (USB Audio Device) ", ""), None).target, device(false, "Microphone (USB Audio Device)"));
        assert_eq!(spec_for(&source(Kind::Output, "", ""), None).target, device(true, ""));
        assert_eq!(spec_for(&source(Kind::Game, "Speakers (USB Audio Device)", ""), None).target, device(true, "Speakers (USB Audio Device)"), "desktop: the output");
        assert_eq!(spec_for(&source(Kind::Game, "", ""), Some(42)).target, MeterTarget::Process { pid: 42 }, "in game: its process");
        assert_eq!(spec_for(&source(Kind::App, "", "player.exe"), None).target, MeterTarget::App { exe: "player.exe".into() });
        let specs = vec![spec_for(&source(Kind::Game, "", ""), None), spec_for(&source(Kind::Input, "", ""), None)];
        let labels: Vec<_> = Shared::new(specs).readings.lock().unwrap().iter().map(|r| r.label.clone()).collect();
        assert_eq!(labels, ["game-1", "input-1"], "readings are keyed by source id");
    }

    #[test]
    fn whole_word_matching_rejects_neighbours() {
        assert!(name_matches("Virtual A (Mixer)", "virtual a"));
        assert!(!name_matches("Virtual B (Mixer)", "virtual a"), "hub bug: 'a' inside another word");
        assert!(name_matches("Microphone (USB Audio Device)", "microphone usb"));
        assert!(!name_matches("Game Capture HD60", "game mixer"));
    }

    /// Same table as `recall` `audio_tests.rs`: the meter picks what the recorder records.
    #[test]
    fn endpoint_matching_is_whole_word_and_folds_the_slashed_o() {
        let names: Vec<String> = ["Virtual B (Mixer)", "Virtual A (Mixer)", "Line (Mixér)", "System (MØNITOR)"].map(String::from).to_vec();
        assert_eq!(match_endpoint(&names, "virtual a"), Some(1), "'a' is not found inside another word");
        assert_eq!(match_endpoint(&names, "System (MØNITOR)"), Some(3), "exact, non-ASCII");
        assert_eq!(match_endpoint(&names, "system monitor"), Some(3), "ø typed as o");
        assert_eq!(match_endpoint(&names, "SYSTEM (mønitor)"), Some(3), "case-insensitive Ø/ø");
        assert_eq!(match_endpoint(&names, "line mixér"), Some(2));
        assert_eq!(match_endpoint(&names, "mix"), None, "a part of a word is not a word");
        assert_eq!(match_endpoint(&names, "  "), None, "empty = Windows default");
    }

    #[test]
    fn levels_compute_rms_and_peak_over_both_channels() {
        let bytes = |samples: &[f32]| samples.iter().flat_map(|s| s.to_le_bytes()).collect::<Vec<u8>>();
        let mut levels = Levels::default();
        levels.push_bytes(&bytes(&[0.5, -0.5, 0.5, -0.5]));
        let (rms, peak) = levels.take();
        assert!((rms - -6.0206).abs() < 0.01 && (peak - -6.0206).abs() < 0.01, "{rms} {peak}");
        levels.push_bytes(&bytes(&[1.0, 0.0, 1.0, 0.0]));
        let (rms, peak) = levels.take();
        assert!((rms - -3.0103).abs() < 0.01 && peak.abs() < 1e-6, "{rms} {peak}");
        assert_eq!(levels.take(), (FLOOR_DB, FLOOR_DB), "empty window is silence");
        levels.push_bytes(&[0, 0, 0]);
        assert_eq!(levels.take(), (FLOOR_DB, FLOOR_DB), "partial sample ignored");
    }

    #[test]
    fn db_floor_and_bar_fraction() {
        assert_eq!(db(0.0), -90.0);
        assert_eq!(db(1e-12), -90.0);
        assert_eq!(db(1.0), 0.0);
        assert!((db(0.1) - -20.0).abs() < 1e-4);
        assert_eq!(bar_fraction(-90.0), 0.0);
        assert_eq!(bar_fraction(-30.0), 0.5);
        assert_eq!(bar_fraction(3.0), 1.0);
    }

    #[test]
    #[ignore = "live hardware: opens shared-mode WASAPI loopback/capture on the default endpoints"]
    fn live_meters_report_every_endpoint() {
        let meters = Meters::spawn(vec![spec_for(&source(Kind::Output, "", ""), None), spec_for(&source(Kind::Input, "", ""), None)]);
        std::thread::sleep(Duration::from_millis(1500));
        let snapshot = meters.snapshot();
        for r in &snapshot {
            eprintln!("{:<10} {:?} rms {:.1} dB peak {:.1} dB", r.label, r.state, r.rms_db, r.peak_db);
        }
        assert_eq!(snapshot.len(), 2);
        assert!(meters.revision() > 0, "threads published readings");
    }

    #[test]
    fn gain_moves_bar_hold_and_number_together_and_keeps_silence() {
        assert_eq!(with_gain(-30.0, 6.0), -24.0);
        assert_eq!(with_gain(-30.0, -12.5), -42.5);
        assert_eq!(with_gain(FLOOR_DB, 12.0), FLOOR_DB, "silence is not lifted to -78 dB");
        assert_eq!(with_gain(-80.0, -60.0), FLOOR_DB, "clamped at the floor");
        assert_eq!(with_gain(-2.0, 12.0), 10.0);
        assert_eq!(with_gain(3.0, 12.0), 12.0, "clamped at the top");
        // A small fader step never makes the bar or the hold line jump: the change is the step.
        for gain in [-60.0_f32, -20.0, 0.0, 6.5, 12.0] {
            let (rms, hold) = (-35.0, -18.0);
            let step = 0.5;
            assert!((bar_fraction(with_gain(rms, gain + step)) - bar_fraction(with_gain(rms, gain))).abs() <= step / 60.0 + 1e-6);
            assert!(with_gain(hold, gain) >= with_gain(rms, gain), "hold stays at or above the bar for gain {gain}");
        }
    }

    #[test]
    fn set_bumps_revision_only_on_visible_change() {
        let shared = Shared::new(vec![spec_for(&source(Kind::Output, "", ""), None)]);
        shared.set(0, -90.0, -90.0, MeterState::Missing);
        assert_eq!(shared.revision.load(Ordering::Acquire), 0);
        shared.set(0, -20.04, -10.0, MeterState::Live);
        shared.set(0, -20.01, -10.0, MeterState::Live);
        assert_eq!(shared.revision.load(Ordering::Acquire), 1);
        assert_eq!(shared.readings.lock().unwrap()[0].rms_db, -20.0);
    }
}
