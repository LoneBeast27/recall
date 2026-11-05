//! Test sound source for app-audio checks: plays a sine tone for a while on ONE named output,
//! so a recording can capture this process by process loopback.
//!
//! It never uses the Windows default output: the output must be named and must match, or it
//! exits without playing. Pick an output with no speakers behind it (a virtual device), so the
//! check is silent for whoever sits at the PC.
//!
//!   tone_player --device "<name words>" --hz 440 --seconds 30 [--amplitude 0.25]
//!
//! Copy the exe under different names to give a check several "apps".
use std::time::{Duration, Instant};

use wasapi::{DeviceCollection, Direction, SampleType, StreamMode, WaveFormat};

fn arg(name: &str) -> Option<String> {
    let args: Vec<String> = std::env::args().collect();
    args.iter().position(|a| a == name).and_then(|i| args.get(i + 1).cloned())
}

fn main() {
    let Some(want) = arg("--device").filter(|d| !d.trim().is_empty()) else {
        eprintln!("--device is required: this player never uses the default output");
        std::process::exit(2);
    };
    let hz: f32 = arg("--hz").and_then(|v| v.parse().ok()).unwrap_or(440.0);
    let seconds: f64 = arg("--seconds").and_then(|v| v.parse().ok()).unwrap_or(30.0);
    let amplitude: f32 = arg("--amplitude").and_then(|v| v.parse().ok()).unwrap_or(0.25);
    if let Err(e) = play(&want, hz, seconds, amplitude) {
        eprintln!("tone_player: {e}");
        std::process::exit(1);
    }
}

fn play(want: &str, hz: f32, seconds: f64, amplitude: f32) -> Result<(), String> {
    let _ = wasapi::initialize_mta();
    let devices = DeviceCollection::new(&Direction::Render).map_err(|e| e.to_string())?;
    let count = devices.get_nbr_devices().map_err(|e| e.to_string())?;
    let tokens: Vec<String> = want.split_whitespace().map(str::to_lowercase).collect();
    let device = (0..count)
        .filter_map(|i| devices.get_device_at_index(i).ok())
        .find(|d| d.get_friendlyname().is_ok_and(|n| tokens.iter().all(|t| n.to_lowercase().contains(t))))
        .ok_or_else(|| format!("no output matches {want:?}; nothing played"))?;
    println!("{{\"playing\":{hz},\"device\":{:?},\"pid\":{}}}", device.get_friendlyname().unwrap_or_default(), std::process::id());
    let mut client = device.get_iaudioclient().map_err(|e| e.to_string())?;
    let (rate, channels) = (48_000usize, 2usize);
    let format = WaveFormat::new(32, 32, &SampleType::Float, rate, channels, None);
    let (_, period) = client.get_device_period().map_err(|e| e.to_string())?;
    let mode = StreamMode::EventsShared { autoconvert: true, buffer_duration_hns: period };
    client.initialize_client(&format, &Direction::Render, &mode).map_err(|e| e.to_string())?;
    let event = client.set_get_eventhandle().map_err(|e| e.to_string())?;
    let render = client.get_audiorenderclient().map_err(|e| e.to_string())?;
    client.start_stream().map_err(|e| e.to_string())?;
    let (start, mut n) = (Instant::now(), 0u64);
    while start.elapsed() < Duration::from_secs_f64(seconds) {
        let frames = client.get_available_space_in_frames().map_err(|e| e.to_string())? as usize;
        let mut bytes = Vec::with_capacity(frames * channels * 4);
        for _ in 0..frames {
            let s = (n as f32 / rate as f32 * hz * std::f32::consts::TAU).sin() * amplitude;
            n += 1;
            for _ in 0..channels {
                bytes.extend_from_slice(&s.to_le_bytes());
            }
        }
        render.write_to_device(frames, &bytes, None).map_err(|e| e.to_string())?;
        let _ = event.wait_for_event(1000);
    }
    let _ = client.stop_stream();
    Ok(())
}
