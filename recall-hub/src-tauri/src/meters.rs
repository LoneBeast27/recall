//! Per-endpoint VU meters: one WASAPI capture thread per known cable emitting
//! `vu` events (~10 Hz, RMS + peak in dBFS). Loopback for render endpoints,
//! plain capture for the mic — same proven pattern as the engine's audio.rs.

use std::io::ErrorKind;

use tauri::{AppHandle, Emitter};
use wasapi::{DeviceCollection, Direction, SampleType, StreamMode, WaveFormat};

/// (label, direction, name-tokens — every token must appear in the friendly name)
const METERS: &[(&str, bool, &str)] = &[
    ("Game", true, "game rødecaster"),
    ("Music", true, "music rødecaster"),
    ("System", true, "system rødecaster"),
    ("Virtual A", true, "virtual a rødecaster"),
    ("Virtual B", true, "virtual b rødecaster"),
    ("Mic", false, "microphone rodecaster chat"),
];

fn name_matches(name: &str, want: &str) -> bool {
    let name = name.to_lowercase();
    want.split_whitespace().all(|t| name.contains(&t.to_lowercase()))
}

pub fn spawn_meters(app: AppHandle) {
    for (label, is_render, tokens) in METERS {
        let app = app.clone();
        std::thread::Builder::new()
            .name(format!("vu-{label}"))
            .spawn(move || meter_thread(app, label, *is_render, tokens))
            .ok();
    }
}

fn db(v: f32) -> f32 {
    if v <= 1e-9 {
        -90.0
    } else {
        (20.0 * v.log10()).max(-90.0)
    }
}

fn meter_thread(app: AppHandle, label: &str, is_render: bool, tokens: &str) {
    let _ = wasapi::initialize_mta();
    let direction = if is_render { Direction::Render } else { Direction::Capture };
    let Ok(collection) = DeviceCollection::new(&direction) else { return };
    let n = collection.get_nbr_devices().unwrap_or(0);
    let device = (0..n)
        .filter_map(|i| collection.get_device_at_index(i).ok())
        .find(|d| d.get_friendlyname().is_ok_and(|nm| name_matches(&nm, tokens)));
    let Some(device) = device else {
        let _ = app.emit("vu", serde_json::json!({ "label": label, "missing": true }));
        return;
    };

    let run = || -> Result<(), String> {
        let mut client = device.get_iaudioclient().map_err(|e| e.to_string())?;
        let format = WaveFormat::new(32, 32, &SampleType::Float, 48000, 2, None);
        let (_d, min_period) = client.get_device_period().map_err(|e| e.to_string())?;
        client
            .initialize_client(
                &format,
                &Direction::Capture,
                &StreamMode::EventsShared {
                    autoconvert: true,
                    buffer_duration_hns: min_period,
                },
            )
            .map_err(|e| e.to_string())?;
        let event = client.set_get_eventhandle().map_err(|e| e.to_string())?;
        let capture = client.get_audiocaptureclient().map_err(|e| e.to_string())?;
        client.start_stream().map_err(|e| e.to_string())?;

        let mut raw: std::collections::VecDeque<u8> = std::collections::VecDeque::new();
        let mut acc_sq = 0f64;
        let mut acc_peak = 0f32;
        let mut acc_n = 0usize;
        let mut last_emit = std::time::Instant::now();
        loop {
            // Loopback streams go silent (no events) when nothing renders —
            // treat timeouts as silence so the meter decays instead of freezing.
            let timed_out = event.wait_for_event(200).is_err();
            if !timed_out && capture.read_from_device_to_deque(&mut raw).is_err() {
                return Err("read failed".into());
            }
            let full = raw.len() - raw.len() % 4;
            for chunk in raw.drain(..full).collect::<Vec<u8>>().chunks_exact(4) {
                let s = f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]);
                acc_sq += (s as f64) * (s as f64);
                acc_peak = acc_peak.max(s.abs());
                acc_n += 1;
            }
            if last_emit.elapsed().as_millis() >= 100 {
                let rms = if acc_n > 0 { ((acc_sq / acc_n as f64) as f32).sqrt() } else { 0.0 };
                let _ = app.emit(
                    "vu",
                    serde_json::json!({
                        "label": label,
                        "rms_db": db(rms),
                        "peak_db": db(acc_peak),
                    }),
                );
                acc_sq = 0.0;
                acc_peak = 0.0;
                acc_n = 0;
                last_emit = std::time::Instant::now();
            }
        }
    };
    if let Err(e) = run() {
        let _ = app.emit(
            "vu",
            serde_json::json!({ "label": label, "error": e, "missing": true }),
        );
        let _ = ErrorKind::Other; // keep the import shape stable
    }
}
