//! RØDECaster level meters (WASAPI loopback on render endpoints, capture on the mic).
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
    /// "Game", "Music", "System", "Virtual A", "Virtual B", "Mic"
    pub label: String,
    pub rms_db: f32,
    pub peak_db: f32,
    pub state: MeterState,
}

/// (label, render endpoint (loopback) vs capture, words that must all appear in the
/// friendly name). Real names: "Game (RØDECaster)", "Virtual A (RØDECaster)",
/// "Microphone (3- RODECaster Duo Chat)". Matching is per whole word so "virtual a"
/// cannot match "Virtual B (RØDECaster)" through the 'a' inside "caster".
const METERS: [(&str, bool, &str); 6] = [
    ("Game", true, "game rødecaster"),
    ("Music", true, "music rødecaster"),
    ("System", true, "system rødecaster"),
    ("Virtual A", true, "virtual a rødecaster"),
    ("Virtual B", true, "virtual b rødecaster"),
    ("Mic", false, "microphone rodecaster chat"),
];
const FLOOR_DB: f32 = -90.0;
const RETRY: Duration = Duration::from_secs(3);
const UPDATE: Duration = Duration::from_millis(16);

pub struct Meters {
    shared: Arc<Shared>,
}

struct Shared {
    readings: Mutex<Vec<MeterReading>>,
    revision: AtomicU64,
    stop: AtomicBool,
}

impl Shared {
    fn new() -> Self {
        let readings = METERS.iter().map(|(label, _, _)| reading(label, FLOOR_DB, FLOOR_DB, MeterState::Missing)).collect();
        Self { readings: Mutex::new(readings), revision: AtomicU64::new(0), stop: AtomicBool::new(false) }
    }

    /// Stores a reading; bumps the revision only when something visible changed.
    fn set(&self, index: usize, rms_db: f32, peak_db: f32, state: MeterState) {
        let next = reading(METERS[index].0, round_tenth(rms_db), round_tenth(peak_db), state);
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
    /// Starts one thread per meter. Never blocks.
    pub fn spawn() -> Self {
        let shared = Arc::new(Shared::new());
        for index in 0..METERS.len() {
            let worker = shared.clone();
            let name = format!("vu-{}", METERS[index].0);
            if let Err(error) = std::thread::Builder::new().name(name).spawn(move || meter_thread(&worker, index)) {
                shared.set(index, FLOOR_DB, FLOOR_DB, MeterState::Error(format!("meter thread: {error}")));
            }
        }
        Self { shared }
    }
    /// Latest readings in display order (always 6 entries).
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
    let (_, is_render, tokens) = METERS[index];
    while !shared.stopped() {
        let state = match find_device(is_render, tokens) {
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

fn find_device(is_render: bool, tokens: &str) -> Result<Option<Device>, String> {
    let direction = if is_render { Direction::Render } else { Direction::Capture };
    let collection = DeviceCollection::new(&direction).map_err(|error| error.to_string())?;
    let count = collection.get_nbr_devices().map_err(|error| error.to_string())?;
    Ok((0..count)
        .filter_map(|i| collection.get_device_at_index(i).ok())
        .find(|device| device.get_friendlyname().is_ok_and(|name| name_matches(&name, tokens))))
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

/// Lowercase, ø→o (so "RØDECaster" and "RODECaster" are one spelling), split into words.
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

    fn tokens_of(label: &str) -> &'static str {
        METERS.iter().find(|(l, _, _)| *l == label).unwrap().2
    }

    #[test]
    fn labels_are_exact_and_in_display_order() {
        let labels: Vec<_> = Shared::new().readings.lock().unwrap().iter().map(|r| r.label.clone()).collect();
        assert_eq!(labels, ["Game", "Music", "System", "Virtual A", "Virtual B", "Mic"]);
        assert!(METERS[..5].iter().all(|(_, render, _)| *render) && !METERS[5].1);
    }

    #[test]
    fn matching_uses_real_endpoint_names_and_both_spellings() {
        assert!(name_matches("Game (RØDECaster)", tokens_of("Game")));
        assert!(name_matches("Game (RODECaster)", tokens_of("Game")));
        assert!(name_matches("Music (RØDECaster)", tokens_of("Music")));
        assert!(name_matches("System (RØDECaster)", tokens_of("System")));
        assert!(name_matches("Virtual A (RØDECaster)", tokens_of("Virtual A")));
        assert!(name_matches("Virtual B (RØDECaster)", tokens_of("Virtual B")));
        assert!(name_matches("Microphone (3- RODECaster Duo Chat)", tokens_of("Mic")));
        assert!(name_matches("Microphone (RØDECaster Duo Chat)", tokens_of("Mic")));
    }

    #[test]
    fn matching_rejects_neighbours() {
        assert!(!name_matches("Virtual B (RØDECaster)", tokens_of("Virtual A")), "hub bug: 'a' inside 'caster'");
        assert!(!name_matches("Virtual A (RØDECaster)", tokens_of("Virtual B")));
        assert!(!name_matches("Speakers (3- RODECaster Duo Chat)", tokens_of("Game")));
        assert!(!name_matches("Microphone (3- RODECaster Duo Main Multitrack)", tokens_of("Mic")));
        assert!(!name_matches("Game Capture HD60", tokens_of("Game")));
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
    #[ignore = "live hardware: opens shared-mode WASAPI loopback/capture on the RØDECaster endpoints"]
    fn live_meters_report_every_endpoint() {
        let meters = Meters::spawn();
        std::thread::sleep(Duration::from_millis(1500));
        let snapshot = meters.snapshot();
        for r in &snapshot {
            eprintln!("{:<10} {:?} rms {:.1} dB peak {:.1} dB", r.label, r.state, r.rms_db, r.peak_db);
        }
        assert_eq!(snapshot.len(), 6);
        assert!(meters.revision() > 0, "threads published readings");
    }

    #[test]
    fn set_bumps_revision_only_on_visible_change() {
        let shared = Shared::new();
        shared.set(0, -90.0, -90.0, MeterState::Missing);
        assert_eq!(shared.revision.load(Ordering::Acquire), 0);
        shared.set(0, -20.04, -10.0, MeterState::Live);
        shared.set(0, -20.01, -10.0, MeterState::Live);
        assert_eq!(shared.revision.load(Ordering::Acquire), 1);
        assert_eq!(shared.readings.lock().unwrap()[0].rms_db, -20.0);
    }
}
