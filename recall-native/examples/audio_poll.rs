//! Dev proof that a process tree makes no sound. Reads what the tree sends to Windows (process
//! loopback, before any device or volume) until the `--stop` file exists, then prints one JSON
//! line. Peak 0 over real frames means nothing could reach a speaker.
//!
//!     cargo run --release --example audio_poll -- --pid <pid> --stop <file>
use std::collections::VecDeque;
use std::path::PathBuf;

use serde_json::json;
use wasapi::{AudioClient, Direction, SampleType, StreamMode, WaveFormat};

const BYTES_PER_SAMPLE: usize = 4;

/// Biggest absolute sample, and how many samples are not zero.
fn measure(bytes: &[u8]) -> (f32, u64) {
    let samples = bytes.chunks_exact(BYTES_PER_SAMPLE).map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]]));
    samples.fold((0.0, 0), |(peak, loud), s| (f32::max(peak, s.abs()), loud + u64::from(s != 0.0)))
}

fn poll(pid: u32, stop: &PathBuf) -> Result<serde_json::Value, String> {
    let text = |error: wasapi::WasapiError| error.to_string();
    let _ = wasapi::initialize_mta();
    let format = WaveFormat::new(32, 32, &SampleType::Float, 48_000, 2, None);
    let mut client = AudioClient::new_application_loopback_client(pid, true).map_err(text)?;
    // 20 ms: the same event period the engine uses for process loopback.
    let mode = StreamMode::EventsShared { autoconvert: true, buffer_duration_hns: 200_000 };
    client.initialize_client(&format, &Direction::Capture, &mode).map_err(text)?;
    let event = client.set_get_eventhandle().map_err(text)?;
    let capture = client.get_audiocaptureclient().map_err(text)?;
    client.start_stream().map_err(text)?;
    println!("{}", json!({ "polling": pid }));

    let (mut peak, mut nonzero, mut bytes) = (0.0f32, 0u64, 0u64);
    let mut raw = VecDeque::new();
    while !stop.exists() {
        if event.wait_for_event(200).is_err() {
            continue;
        }
        capture.read_from_device_to_deque(&mut raw).map_err(text)?;
        let full = raw.len() - raw.len() % BYTES_PER_SAMPLE;
        let block: Vec<u8> = raw.drain(..full).collect();
        let (block_peak, block_nonzero) = measure(&block);
        peak = peak.max(block_peak);
        nonzero += block_nonzero;
        bytes += block.len() as u64;
    }
    let frames = bytes / (2 * BYTES_PER_SAMPLE as u64);
    Ok(json!({ "pid": pid, "seconds": frames as f64 / 48_000.0, "peak": peak, "nonzero_samples": nonzero }))
}

fn main() -> Result<(), String> {
    let args: Vec<String> = std::env::args().collect();
    let value = |name: &str| args.iter().position(|a| a == name).and_then(|i| args.get(i + 1));
    let pid = value("--pid").and_then(|v| v.parse().ok()).ok_or("--pid <pid> is required")?;
    let stop = PathBuf::from(value("--stop").ok_or("--stop <file> is required")?);
    println!("{}", poll(pid, &stop)?);
    Ok(())
}
