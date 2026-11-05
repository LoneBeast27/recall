//! Level meters for the sources clips record: the desktop audio output (WASAPI loopback) and
//! the microphone (capture), each the device chosen in Settings or the Windows default.
//! Ported from the hub's `meters.rs`; missing devices are retried.
use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use wasapi::{Device, DeviceCollection, Direction, SampleType, StreamMode, WaveFormat};

#[derive(Debug, Clone, PartialEq)]
pub enum MeterState {
    Live,
    Missing,
    Error(String),
}

#[derive(Debug, Clone, PartialEq)]
pub struct MeterReading {
    /// "Desktop audio" or "Microphone".
    pub label: String,
    pub rms_db: f32,
    pub peak_db: f32,
    pub state: MeterState,
}

/// One meter: a label, render endpoint (loopback) vs capture, and the endpoint's friendly
/// name ("" = the Windows default for that direction).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MeterSpec {
    pub label: &'static str,
    pub render: bool,
    pub device: String,
}

/// The meters for the recording settings: desktop audio always, the microphone only when it is
/// recorded.
pub fn meter_specs(desktop_device: &str, mic_device: &str, mic_enabled: bool) -> Vec<MeterSpec> {
    let mut specs = vec![MeterSpec { label: "Desktop audio", render: true, device: desktop_device.trim().to_owned() }];
    if mic_enabled {
        specs.push(MeterSpec { label: "Microphone", render: false, device: mic_device.trim().to_owned() });
    }
    specs
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
        let readings = specs.iter().map(|spec| reading(spec.label, FLOOR_DB, FLOOR_DB, MeterState::Missing)).collect();
        Self { specs, readings: Mutex::new(readings), revision: AtomicU64::new(0), stop: AtomicBool::new(false) }
    }

    /// Stores a reading; bumps the revision only when something visible changed.
    fn set(&self, index: usize, rms_db: f32, peak_db: f32, state: MeterState) {
        let next = reading(self.specs[index].label, round_tenth(rms_db), round_tenth(peak_db), state);
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
        let state = match find_device(spec.render, &spec.device) {
            Ok(Some(device)) => match capture(shared, index, &device) {
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

/// The named endpoint (exact name first, then whole-word match, like the recorder), or the
/// Windows default when `name` is "". A named device that is gone reads as missing: the meter
/// never shows a different device than the one being recorded.
fn find_device(is_render: bool, name: &str) -> Result<Option<Device>, String> {
    let direction = if is_render { Direction::Render } else { Direction::Capture };
    if name.is_empty() {
        return Ok(wasapi::get_default_device(&direction).ok());
    }
    let collection = DeviceCollection::new(&direction).map_err(|error| error.to_string())?;
    let count = collection.get_nbr_devices().map_err(|error| error.to_string())?;
    let devices: Vec<Device> = (0..count).filter_map(|i| collection.get_device_at_index(i).ok()).collect();
    let names: Vec<String> = devices.iter().map(|d| d.get_friendlyname().unwrap_or_default()).collect();
    let index = names.iter().position(|n| n.trim().eq_ignore_ascii_case(name)).or_else(|| names.iter().position(|n| name_matches(n, name)));
    Ok(index.and_then(|i| devices.into_iter().nth(i)))
}

/// Streams levels at ~10 Hz. Returns Ok(()) only when asked to stop; device loss
/// surfaces as an error from the read or the padding probe on a silent timeout.
fn capture(shared: &Shared, index: usize, device: &Device) -> Result<(), String> {
    let text = |error: wasapi::WasapiError| error.to_string();
    let mut client = device.get_iaudioclient().map_err(text)?;
    let format = WaveFormat::new(32, 32, &SampleType::Float, 48000, 2, None);
    let (_, min_period) = client.get_device_period().map_err(text)?;
    client
        .initialize_client(&format, &Direction::Capture, &StreamMode::EventsShared { autoconvert: true, buffer_duration_hns: min_period })
        .map_err(text)?;
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
    words(tokens).iter().all(|token| name_words.contains(token))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn meters_follow_the_recording_settings() {
        let specs = meter_specs("", " Speakers (USB Audio) ", true);
        assert_eq!(specs.iter().map(|s| s.label).collect::<Vec<_>>(), ["Desktop audio", "Microphone"]);
        assert!(specs[0].render && !specs[1].render);
        assert_eq!((specs[0].device.as_str(), specs[1].device.as_str()), ("", "Speakers (USB Audio)"));
        assert_eq!(meter_specs("", "", false).len(), 1, "no mic meter when the mic is off");
        let labels: Vec<_> = Shared::new(specs).readings.lock().unwrap().iter().map(|r| r.label.clone()).collect();
        assert_eq!(labels, ["Desktop audio", "Microphone"]);
    }

    #[test]
    fn whole_word_matching_rejects_neighbours() {
        assert!(name_matches("Virtual A (Mixer)", "virtual a"));
        assert!(!name_matches("Virtual B (Mixer)", "virtual a"), "hub bug: 'a' inside another word");
        assert!(name_matches("Microphone (USB Audio Device)", "microphone usb"));
        assert!(!name_matches("Game Capture HD60", "game mixer"));
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
        let meters = Meters::spawn(meter_specs("", "", true));
        std::thread::sleep(Duration::from_millis(1500));
        let snapshot = meters.snapshot();
        for r in &snapshot {
            eprintln!("{:<10} {:?} rms {:.1} dB peak {:.1} dB", r.label, r.state, r.rms_db, r.peak_db);
        }
        assert_eq!(snapshot.len(), 2);
        assert!(meters.revision() > 0, "threads published readings");
    }

    #[test]
    fn set_bumps_revision_only_on_visible_change() {
        let shared = Shared::new(meter_specs("", "", false));
        shared.set(0, -90.0, -90.0, MeterState::Missing);
        assert_eq!(shared.revision.load(Ordering::Acquire), 0);
        shared.set(0, -20.04, -10.0, MeterState::Live);
        shared.set(0, -20.01, -10.0, MeterState::Live);
        assert_eq!(shared.revision.load(Ordering::Acquire), 1);
        assert_eq!(shared.readings.lock().unwrap()[0].rms_db, -20.0);
    }
}
